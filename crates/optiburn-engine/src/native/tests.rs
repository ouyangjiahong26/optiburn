use super::*;
use optiburn_mmc::CurrentProfile;
use optiburn_transport::{Completion, Direction, ScsiTransport, TransportError};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// 应答脚本化的假驱动器：按 opcode 回固定响应，记录命令顺序与写入载荷。
struct ScriptedDrive {
    profile: u16,
    disc_status: u8,
    capacity: Option<u32>,
    /// READ TRACK INFORMATION 报的剩余可写块数（容量门禁用）。
    free_blocks: u32,
    /// READ TRACK INFORMATION 报的下一个可写地址（新区段起点）。
    nwa: u32,
    /// READ TRACK INFORMATION 报的开放轨道起始地址；给 None 时与 `nwa` 相同。
    open_track_start: Option<u32>,
    /// 报 Damage 位置位（驱动器认了损坏轨道）。
    damaged: bool,
    /// 报 NWA_V 置位（地址字段可信）。中断刻录的实机形态是置 Damage、清 NWA_V。
    nwa_valid: bool,
    /// READ TOC Format 1 报的末区段起点（旧区段内容的位置）。
    last_session_start: u32,
    /// READ TOC Format 0 报的轨道（编号与起点），候选枚举用它。
    tracks: Vec<(u8, u32)>,
    /// 合成盘：READ(10) 从这里按块取数据，越界报命令失败。
    disc: Vec<u8>,
    log: Rc<RefCell<Vec<Vec<u8>>>>,
    written: Rc<RefCell<Vec<u8>>>,
    cancel_after_first_write: Option<CancelToken>,
}

impl ScriptedDrive {
    fn new(profile: u16, disc_status: u8) -> Self {
        Self {
            profile,
            disc_status,
            capacity: Some(0x10_0000),
            free_blocks: 100_000,
            nwa: 0x2000,
            open_track_start: None,
            damaged: false,
            nwa_valid: true,
            last_session_start: 0x1000,
            tracks: vec![(1, 0), (2, 0x1000)],
            disc: Vec::new(),
            log: Rc::new(RefCell::new(Vec::new())),
            written: Rc::new(RefCell::new(Vec::new())),
            cancel_after_first_write: None,
        }
    }
}

impl ScsiTransport for ScriptedDrive {
    fn issue(
        &mut self,
        cdb: &[u8],
        dir: Direction,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<Completion, TransportError> {
        self.log.borrow_mut().push(cdb.to_vec());
        match cdb[0] {
            0x51 => {
                // READ DISC INFORMATION：状态位在字节 2 的低两位。
                if data.len() >= 34 {
                    data[2] = self.disc_status & 0b11;
                    data[3] = 1;
                    data[4] = 1;
                }
            }
            0x46 => {
                if data.len() >= 8 {
                    data[6] = (self.profile >> 8) as u8;
                    data[7] = self.profile as u8;
                }
            }
            0x25 => match self.capacity {
                Some(last_lba) => {
                    data[..4].copy_from_slice(&last_lba.to_be_bytes());
                    data[4..8].copy_from_slice(&2048u32.to_be_bytes());
                }
                None => {
                    return Err(TransportError::CommandFailed {
                        cdb: cdb.to_vec(),
                        scsi_status: 2,
                        sense: vec![0x70, 0x00, 0x05, 0x20],
                    });
                }
            },
            0x52 => {
                // READ TRACK INFORMATION：起始地址 8-11，NWA 12-15，剩余块数 16-19，
                // Damage 位在字节 5 的 bit 5、NWA_V 在字节 7 的 bit 0（libburn 的
                // `mmc_get_nwa` 按这两位移流）。默认报健康：地址有效、没有损坏。
                if data.len() >= 20 {
                    let start = self.open_track_start.unwrap_or(self.nwa);
                    data[5] = if self.damaged { 0x20 } else { 0x00 };
                    data[7] = u8::from(self.nwa_valid);
                    data[8..12].copy_from_slice(&start.to_be_bytes());
                    data[12..16].copy_from_slice(&self.nwa.to_be_bytes());
                    data[16..20].copy_from_slice(&self.free_blocks.to_be_bytes());
                }
            }
            0x43 => match cdb[2] & 0x0F {
                // Format 0：轨道列表（候选枚举用），起点由测试给定。
                0 => {
                    let length = 2 + self.tracks.len() * 8;
                    if data.len() >= 4 + self.tracks.len() * 8 {
                        data[0] = (length >> 8) as u8;
                        data[1] = length as u8;
                        data[2] = self.tracks.first().map(|t| t.0).unwrap_or(1);
                        data[3] = self.tracks.last().map(|t| t.0).unwrap_or(1);
                        for (index, (track, lba)) in self.tracks.iter().enumerate() {
                            let offset = 4 + index * 8;
                            data[offset + 1] = 0x14;
                            data[offset + 2] = *track;
                            data[offset + 4..offset + 8].copy_from_slice(&lba.to_be_bytes());
                        }
                    }
                }
                // Format 1：数据长度 10，首末会话编号 1 与 2，
                // 描述符的字节 4-7（响应 8-11）是末区段起始地址。
                _ => {
                    if data.len() >= 12 {
                        data[0] = 0;
                        data[1] = 10;
                        data[2] = 1;
                        data[3] = 2;
                        data[4] = 1;
                        data[8..12].copy_from_slice(&self.last_session_start.to_be_bytes());
                    }
                }
            },
            0x28 if dir == Direction::FromDevice => {
                let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]) as usize;
                let start = lba * SECTOR_BYTES;
                let end = start + data.len();
                match self.disc.get(start..end) {
                    Some(bytes) => data.copy_from_slice(bytes),
                    None => {
                        return Err(TransportError::CommandFailed {
                            cdb: cdb.to_vec(),
                            scsi_status: 2,
                            sense: vec![0x70, 0x00, 0x05, 0x21],
                        });
                    }
                }
            }
            0x2A if dir == Direction::ToDevice => {
                self.written.borrow_mut().extend_from_slice(data);
                if let Some(token) = self.cancel_after_first_write.take() {
                    token.cancel();
                }
            }
            _ => {}
        }
        Ok(Completion {
            scsi_status: 0,
            sense: Vec::new(),
            residual: 0,
        })
    }

    fn device_path(&self) -> &str {
        "/dev/fake"
    }
}

/// 写一个临时镜像文件，内容为 `data`，返回路径与清理用的守卫。
struct TempImage {
    path: PathBuf,
}

impl TempImage {
    fn new(name: &str, data: &[u8]) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("optiburn-native-{name}-{}.iso", std::process::id()));
        std::fs::write(&path, data).expect("write temp image");
        Self { path }
    }
}

impl Drop for TempImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn job(image: PathBuf) -> BurnJob {
    BurnJob {
        image,
        device: "/dev/fake".into(),
        speed: None,
        multi: true,
    }
}

/// 跑一次完整写盘，返回命令顺序与最后一次进度。取消钩子不挂：需要取消的
/// 用例自己建设备并挂令牌。
fn run(mut drive: ScriptedDrive, image: &TempImage) -> Result<(Vec<Vec<u8>>, f32), BurnError> {
    let log = Rc::clone(&drive.log);
    drive.cancel_after_first_write = None;
    let mut mmc = MmcDevice::new(Box::new(drive));
    let mut last = 0.0f32;
    let result = burn_with_device(
        &mut mmc,
        &job(image.path.clone()),
        &mut |f| last = f,
        &CancelToken::default(),
    );
    let cdbs = log.borrow().clone();
    result.map(|()| (cdbs, last))
}

fn opcodes(cdbs: &[Vec<u8>]) -> Vec<u8> {
    cdbs.iter().map(|cdb| cdb[0]).collect()
}

#[test]
fn blank_cd_burn_writes_the_whole_sequence() {
    let image = TempImage::new("cd", &vec![0xAAu8; 3 * SECTOR_BYTES + 100]);
    let drive = ScriptedDrive::new(CurrentProfile::CD_R, 0);
    let written = Rc::clone(&drive.written);

    let (cdbs, last) = run(drive, &image).expect("burn succeeds");

    // 顺序：就绪、读盘片信息、读 Profile、写参数、写、冲刷、关区段，
    // 关区段之后再轮询就绪（TUR 0x00）。CD 不读容量、不预留轨道（真机实测，
    // 见 ADR-0017 补记）。
    let ops = opcodes(&cdbs);
    assert!(
        ops.starts_with(&[0x00, 0x51, 0x46, 0x55, 0x2A, 0x35, 0x5B]),
        "{ops:?}"
    );
    assert!(
        ops[7..].iter().all(|op| *op == 0x00),
        "关区段后只应轮询就绪: {ops:?}"
    );

    // 数据逐块写出，末块少于一块时补零。
    let filled = 3 * SECTOR_BYTES + 100;
    let data = written.borrow();
    assert_eq!(data.len(), 4 * SECTOR_BYTES);
    assert!(data[..filled].iter().all(|b| *b == 0xAA), "镜像原样写出");
    assert!(
        data[filled..].iter().all(|b| *b == 0),
        "末块补零到整块（多出 {} 字节）",
        data.len() - filled
    );
    assert_eq!(last, 1.0, "进度收在 1.0");
}

#[test]
fn appendable_disc_is_written_as_a_new_session_at_nwa() {
    let image = TempImage::new("appendable", &vec![0x22u8; 2 * SECTOR_BYTES]);
    let drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b01);
    let written = Rc::clone(&drive.written);

    let (cdbs, last) = run(drive, &image).expect("burn succeeds");

    let ops = opcodes(&cdbs);
    assert!(ops.contains(&0x52), "可追加盘要先问 NWA: {ops:?}");
    let write = cdbs
        .iter()
        .find(|cdb| cdb[0] == 0x2A)
        .expect("a WRITE(10) was issued");
    let lba = u32::from_be_bytes([write[2], write[3], write[4], write[5]]);
    assert_eq!(lba, 0x2000, "从 NWA 开始写新区段");
    assert_eq!(written.borrow().len(), 2 * SECTOR_BYTES);
    assert_eq!(last, 1.0);
}

#[test]
fn finalized_disc_is_refused() {
    let image = TempImage::new("finalized", &vec![0u8; SECTOR_BYTES]);
    let drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b10);

    let err = run(drive, &image).expect_err("finalized disc must be refused");
    assert!(
        matches!(err, BurnError::NativeGap(NativeGap::FinalizedDisc)),
        "{err:?}"
    );
}

#[test]
fn unknown_profile_is_refused() {
    let image = TempImage::new("unknown", &vec![0u8; SECTOR_BYTES]);
    let drive = ScriptedDrive::new(0xFFFF, 0);

    let err = run(drive, &image).expect_err("unknown profile must be refused");
    assert!(
        matches!(
            err,
            BurnError::NativeGap(NativeGap::UnsupportedProfile(0xFFFF))
        ),
        "{err:?}"
    );
}

#[test]
fn dvd_ram_skips_write_parameters_and_reserve_track() {
    let image = TempImage::new("ram", &vec![0x11u8; SECTOR_BYTES]);
    let drive = ScriptedDrive::new(CurrentProfile::DVD_RAM, 0b11);

    let (cdbs, last) = run(drive, &image).expect("burn succeeds");
    let ops = opcodes(&cdbs);
    assert!(!ops.contains(&0x53), "随机可写介质不预留轨道: {ops:?}");
    assert!(
        !ops.contains(&0x55),
        "不写参数的介质组不发 MODE SELECT: {ops:?}"
    );
    assert!(!ops.contains(&0x5B), "随机可写介质不关区段: {ops:?}");
    assert_eq!(ops.iter().filter(|op| **op == 0x2A).count(), 1, "{ops:?}");
    assert_eq!(last, 1.0);
}

#[test]
fn cancel_between_chunks_stops_the_burn() {
    // 40 块超过单条 WRITE 的 32 块，会分成两块来写，取消正好落在两块之间。
    let image = TempImage::new("cancel", &vec![0u8; 40 * SECTOR_BYTES]);
    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0);
    let written_blocks = Rc::clone(&drive.written);
    let token = CancelToken::new();
    // 第一条 WRITE 之后假驱动器置位令牌，模拟用户在写盘中途点中止。
    drive.cancel_after_first_write = Some(token.clone());

    let mut mmc = MmcDevice::new(Box::new(drive));
    let mut progress_calls = 0;
    let result = burn_with_device(
        &mut mmc,
        &job(image.path.clone()),
        &mut |_| progress_calls += 1,
        &token,
    );

    assert!(matches!(result, Err(BurnError::Cancelled)), "{result:?}");
    // 第一块写完之后令牌被置位，第二块不再下发。
    assert_eq!(written_blocks.borrow().len(), 32 * SECTOR_BYTES);
    assert!(progress_calls <= 2, "取消后不再推进进度");
}

#[test]
fn speed_option_is_refused() {
    let image = TempImage::new("speed", &vec![0u8; SECTOR_BYTES]);
    let drive = ScriptedDrive::new(CurrentProfile::CD_R, 0);
    let mut mmc = MmcDevice::new(Box::new(drive));
    let mut job = job(image.path.clone());
    job.speed = Some(8);

    let err = burn_with_device(&mut mmc, &job, &mut |_| {}, &CancelToken::default())
        .expect_err("speed is not implemented");
    assert!(
        matches!(err, BurnError::NativeGap(NativeGap::WriteSpeed)),
        "{err:?}"
    );
}

#[test]
fn image_larger_than_media_is_refused() {
    // 容量预检只对非 CD 介质生效（CD 的两个地址空间不能混比），用 DVD-R 试。
    let image = TempImage::new("toobig", &vec![0u8; 4 * SECTOR_BYTES]);
    let mut drive = ScriptedDrive::new(CurrentProfile::DVD_R, 0);
    drive.capacity = Some(2); // 介质只有 3 块，镜像是 4 块
    let written = Rc::clone(&drive.written);

    let mut mmc = MmcDevice::new(Box::new(drive));
    let err = burn_with_device(
        &mut mmc,
        &job(image.path.clone()),
        &mut |_| {},
        &CancelToken::default(),
    )
    .expect_err("image must not fit");
    assert!(
        matches!(err, BurnError::NativeGap(NativeGap::ImageTooLarge)),
        "{err:?}"
    );
    assert!(written.borrow().is_empty(), "拒绝发生在任何写入之前");
}

#[test]
fn capacity_probe_failure_does_not_block_the_burn() {
    let image = TempImage::new("nocap", &vec![0u8; SECTOR_BYTES]);
    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0);
    drive.capacity = None; // 空白 CD 上 READ CAPACITY 可能不支持

    let (_, last) = run(drive, &image).expect("burn proceeds without capacity info");
    assert_eq!(last, 1.0);
}

/// 临时源目录守卫（增长模式的源是目录）。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "optiburn-native-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn grow_job(src: &Path) -> GrowJob {
    GrowJob {
        src: src.to_path_buf(),
        device: "/dev/fake".into(),
        speed: None,
        volume_id: "GROWTV1".into(),
        close_disc: false,
        allow_damaged_last_session: false,
    }
}

/// 跑一次增长写盘，返回命令顺序与假驱动器收到的写入载荷。
fn run_grow(drive: ScriptedDrive, src: &Path) -> Result<(Vec<Vec<u8>>, Vec<u8>), BurnError> {
    run_grow_job(drive, &grow_job(src))
}

/// 跑一次增长写盘，用调用方给的任务（需要 `allow_damaged_last_session` 这类非默认
/// 取值时用这个）。
fn run_grow_job(
    mut drive: ScriptedDrive,
    job: &GrowJob,
) -> Result<(Vec<Vec<u8>>, Vec<u8>), BurnError> {
    let log = Rc::clone(&drive.log);
    let written = Rc::clone(&drive.written);
    drive.cancel_after_first_write = None;
    let mut mmc = MmcDevice::new(Box::new(drive));
    let result = grow_with_device(&mut mmc, job, &mut |_| {}, &CancelToken::default());
    let cdbs = log.borrow().clone();
    result.map(|()| (cdbs, written.borrow().clone()))
}

/// 用增长读侧把一段会话字节读成树。
fn read_session(image: Vec<u8>, base: u32) -> crate::grow::OldSession {
    let blocks = image;
    let mut read = |lba: u32, out: &mut [u8]| {
        let start = lba as usize * SECTOR_BYTES;
        let end = start + out.len();
        let Some(bytes) = blocks.get(start..end) else {
            return Err(BurnError::ReadFailed(format!("读越界：第 {lba} 块")));
        };
        out.copy_from_slice(bytes);
        Ok(())
    };
    crate::grow::read_old_session(&mut read, base, &CancelToken::default())
        .expect("read the written session")
}

fn names(node: &crate::grow::DirNode) -> Vec<String> {
    let mut out: Vec<String> = node.files.iter().map(|file| file.name.clone()).collect();
    out.extend(node.dirs.iter().map(|dir| dir.name.clone()));
    out.sort();
    out
}

#[test]
fn grow_refuses_a_damaged_last_session_without_confirmation() {
    // 盘：[起点 0x1000 的完好会话][起点 0x1800 的残片]，TOC 把残片报成末区段。
    let tmp = TempDir::new("grow-damaged");
    let old_src = tmp.path().join("old");
    std::fs::create_dir_all(&old_src).expect("create old src");
    std::fs::write(old_src.join("old.txt"), b"old content").expect("write old file");
    let old_plan = crate::grow::plan_session(None, &old_src, "OLD".into(), 0x1000)
        .expect("plan the old session");
    let mut old_image = Vec::new();
    old_plan
        .write_image(&mut old_image, &CancelToken::default())
        .expect("write the old session");
    let mut disc = vec![0u8; 0x3000 * SECTOR_BYTES];
    disc[0x1000 * SECTOR_BYTES..0x1000 * SECTOR_BYTES + old_image.len()]
        .copy_from_slice(&old_image);
    // 残片：0x1800 处放一段不是 ISO 描述符区的字节。
    disc[0x1800 * SECTOR_BYTES..0x1801 * SECTOR_BYTES].fill(0x55);

    let new_src = tmp.path().join("new");
    std::fs::create_dir_all(&new_src).expect("create new src");
    std::fs::write(new_src.join("new.txt"), b"new content").expect("write new file");

    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b01);
    drive.disc = disc.clone();
    drive.last_session_start = 0x1800;
    drive.tracks = vec![(1, 0x1000), (2, 0x1800), (0xAA, 0x1900)];
    let mut mmc = MmcDevice::new(Box::new(drive));
    let error = grow_with_device(
        &mut mmc,
        &grow_job(&new_src),
        &mut |_| {},
        &CancelToken::default(),
    )
    .expect_err("没有确认就该拒绝");
    assert!(
        matches!(
            error,
            BurnError::DamagedLastSession { fallback }
                if fallback.skipped == 1
                    && fallback.ordinal == 1
                    && fallback.candidates == 2
                    && fallback.session_start == 0x1000
        ),
        "{error:?}"
    );
}

#[test]
fn grow_refuses_a_missing_write_address_without_confirmation() {
    // 中断刻录之后驱动器可能报不出 NWA（实测地址字段全零）：不许拿 0 当起点写。
    // 旧区段本身可读，拒绝发生在解析写入位置这一步（先读旧树、后定地址，读之前
    // 不下发任何写命令）。
    let tmp = TempDir::new("grow-no-nwa");
    let old_src = tmp.path().join("old");
    std::fs::create_dir_all(&old_src).expect("create old src");
    std::fs::write(old_src.join("old.txt"), b"old content").expect("write old file");
    let old_plan = crate::grow::plan_session(None, &old_src, "OLD".into(), 0x1000)
        .expect("plan the old session");
    let mut old_image = Vec::new();
    old_plan
        .write_image(&mut old_image, &CancelToken::default())
        .expect("write the old session");
    let mut disc = vec![0u8; 0x3000 * SECTOR_BYTES];
    disc[0x1000 * SECTOR_BYTES..0x1000 * SECTOR_BYTES + old_image.len()]
        .copy_from_slice(&old_image);

    let new_src = tmp.path().join("new");
    std::fs::create_dir_all(&new_src).expect("create new src");
    std::fs::write(new_src.join("new.txt"), b"new content").expect("write new file");

    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b01);
    drive.disc = disc;
    drive.nwa = 0;
    drive.nwa_valid = false;
    drive.last_session_start = 0x1000;
    drive.tracks = vec![(1, 0), (2, 0x1000), (0xAA, 0x1100)];

    let mut mmc = MmcDevice::new(Box::new(drive));
    let error = grow_with_device(
        &mut mmc,
        &grow_job(&new_src),
        &mut |_| {},
        &CancelToken::default(),
    )
    .expect_err("报不出 NWA 时不该硬写");
    assert!(
        matches!(
            error,
            BurnError::WriteAddressUnknown {
                last_session_start: 0x1000,
                damaged: false
            }
        ),
        "{error:?}"
    );
}

#[test]
fn grow_grafts_onto_the_previous_session_when_confirmed() {
    // 与上一测试同一张盘，带上确认：嫁接源变成 0x1000 的完好会话，新区段仍写在
    // NWA（0x2000），旧文件进新树、被跳过的残片不进。
    let tmp = TempDir::new("grow-damaged-ok");
    let old_src = tmp.path().join("old");
    std::fs::create_dir_all(&old_src).expect("create old src");
    std::fs::write(old_src.join("old.txt"), b"old content").expect("write old file");
    let old_plan = crate::grow::plan_session(None, &old_src, "OLD".into(), 0x1000)
        .expect("plan the old session");
    let mut old_image = Vec::new();
    old_plan
        .write_image(&mut old_image, &CancelToken::default())
        .expect("write the old session");
    let mut disc = vec![0u8; 0x3000 * SECTOR_BYTES];
    disc[0x1000 * SECTOR_BYTES..0x1000 * SECTOR_BYTES + old_image.len()]
        .copy_from_slice(&old_image);
    disc[0x1800 * SECTOR_BYTES..0x1801 * SECTOR_BYTES].fill(0x55);

    let new_src = tmp.path().join("new");
    std::fs::create_dir_all(&new_src).expect("create new src");
    std::fs::write(new_src.join("new.txt"), b"new content").expect("write new file");

    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b01);
    drive.disc = disc.clone();
    drive.last_session_start = 0x1800;
    drive.tracks = vec![(1, 0x1000), (2, 0x1800), (0xAA, 0x1900)];
    let mut job = grow_job(&new_src);
    job.allow_damaged_last_session = true;
    let (_, written) = run_grow_job(drive, &job).expect("确认后应当写成功");

    // 写下的字节拼到自己的起点上读回：旧文件仍在，新文件也在。
    let mut grown = disc;
    grown[0x2000 * SECTOR_BYTES..0x2000 * SECTOR_BYTES + written.len()].copy_from_slice(&written);
    let session = read_session(grown, 0x2000);
    assert_eq!(
        names(&session.root),
        vec!["new.txt".to_string(), "old.txt".to_string()]
    );
}

#[test]
fn grow_refuses_when_nwa_is_invalid_even_if_the_start_address_looks_usable() {
    // 实机形态（2026-10-11）：驱动器报 Damage 位置位、NWA_V 清零，起始地址字段仍
    // 给一个看着合理的值（301170）。libburn 的 mmc_get_nwa 不信这个值，我们同样
    // 不用它当起点。
    let tmp = TempDir::new("grow-open-track");
    let old_src = tmp.path().join("old");
    std::fs::create_dir_all(&old_src).expect("create old src");
    std::fs::write(old_src.join("old.txt"), b"old content").expect("write old file");
    let old_plan = crate::grow::plan_session(None, &old_src, "OLD".into(), 0x1000)
        .expect("plan the old session");
    let mut old_image = Vec::new();
    old_plan
        .write_image(&mut old_image, &CancelToken::default())
        .expect("write the old session");
    let mut disc = vec![0u8; 0x3000 * SECTOR_BYTES];
    disc[0x1000 * SECTOR_BYTES..0x1000 * SECTOR_BYTES + old_image.len()]
        .copy_from_slice(&old_image);

    let new_src = tmp.path().join("new");
    std::fs::create_dir_all(&new_src).expect("create new src");
    std::fs::write(new_src.join("new.txt"), b"new content").expect("write new file");

    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b01);
    drive.disc = disc;
    drive.last_session_start = 0x1000;
    drive.tracks = vec![(1, 0x1000), (2, 0x1200)];
    drive.nwa = 0;
    drive.open_track_start = Some(0x1200);
    drive.damaged = true;
    drive.nwa_valid = false;
    let mut mmc = MmcDevice::new(Box::new(drive));
    let error = grow_with_device(
        &mut mmc,
        &grow_job(&new_src),
        &mut |_| {},
        &CancelToken::default(),
    )
    .expect_err("NWA_V 清零时不认起始地址");
    assert!(
        matches!(
            error,
            BurnError::WriteAddressUnknown {
                last_session_start: 0x1000,
                damaged: true
            }
        ),
        "{error:?}"
    );
}

#[test]
fn grow_on_a_blank_disc_writes_the_first_session() {
    let src = TempDir::new("grow-blank");
    std::fs::write(src.path().join("a.txt"), b"hello").expect("write sample");
    let drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b00);

    let (cdbs, written) = run_grow(drive, src.path()).expect("grow succeeds");

    let ops = opcodes(&cdbs);
    // 就绪、读盘片信息、读 Profile、读剩余容量（TRACK INFO 加 FORMAT
    // CAPACITIES）、写参数、写、冲刷、关区段，之后只轮询就绪。
    assert!(
        ops.starts_with(&[0x00, 0x51, 0x46, 0x52, 0x23, 0x55, 0x2A, 0x35, 0x5B]),
        "{ops:?}"
    );
    assert!(
        ops[9..].iter().all(|op| *op == 0x00),
        "关区段后只应轮询就绪: {ops:?}"
    );
    assert!(!ops.contains(&0x28), "空盘上没有旧区段可读: {ops:?}");
    let write = cdbs
        .iter()
        .find(|cdb| cdb[0] == 0x2A)
        .expect("a WRITE(10) was issued");
    assert_eq!(
        u32::from_be_bytes([write[2], write[3], write[4], write[5]]),
        0,
        "空盘从 LBA 0 起写第一区段"
    );
    // 写下的字节读回来是新会话：卷标与文件都在。
    let session = read_session(written, 0);
    assert_eq!(names(&session.root), vec!["a.txt".to_string()]);
}

#[test]
fn grow_on_an_appendable_disc_grafts_the_old_session() {
    const OLD_BASE: u32 = 0x1000;
    let tmp = TempDir::new("graft");
    let old_src = tmp.path().join("old");
    std::fs::create_dir_all(&old_src).expect("create old src");
    std::fs::write(old_src.join("old.txt"), b"old content").expect("write old file");
    // 旧区段：先用生成器按旧区段起点造一份（等价于上次刻录写下去的东西）。
    let old_plan = crate::grow::plan_session(None, &old_src, "OLD".into(), OLD_BASE)
        .expect("plan the old session");
    let mut old_image = Vec::new();
    old_plan
        .write_image(&mut old_image, &CancelToken::default())
        .expect("write the old session");
    let mut disc = vec![0u8; 0x3000 * SECTOR_BYTES];
    disc[OLD_BASE as usize * SECTOR_BYTES..OLD_BASE as usize * SECTOR_BYTES + old_image.len()]
        .copy_from_slice(&old_image);

    let new_src = tmp.path().join("new");
    std::fs::create_dir_all(&new_src).expect("create new src");
    std::fs::write(new_src.join("new.txt"), b"new content").expect("write new file");

    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b01);
    drive.disc = disc.clone();
    let (cdbs, written) = run_grow(drive, &new_src).expect("grow succeeds");

    let ops = opcodes(&cdbs);
    let first_read = ops.iter().position(|op| *op == 0x28).expect("读取旧区段");
    let first_write = ops.iter().position(|op| *op == 0x2A).expect("写入新区段");
    assert!(ops.contains(&0x43), "旧区段起点问 READ TOC: {ops:?}");
    assert!(first_read < first_write, "先读旧区段再写: {ops:?}");
    // 旧区段的内容在它自己的起点处读（NWA 是新区段的位置，不能用来定位旧区段）。
    let read = &cdbs[first_read];
    assert_eq!(
        u32::from_be_bytes([read[2], read[3], read[4], read[5]]),
        0x1010,
        "从旧区段的描述符区起读"
    );
    let write = &cdbs[first_write];
    assert_eq!(
        u32::from_be_bytes([write[2], write[3], write[4], write[5]]),
        0x2000,
        "可追加盘接在 NWA 后面写"
    );

    // 盘上拼出新区段后读回：旧条目加新文件的合集。
    let mut grown = disc;
    grown[0x2000 * SECTOR_BYTES..0x2000 * SECTOR_BYTES + written.len()].copy_from_slice(&written);
    let session = read_session(grown, 0x2000);
    assert_eq!(
        names(&session.root),
        vec!["new.txt".to_string(), "old.txt".to_string()]
    );
}

#[test]
fn grow_refuses_a_rewritable_disc() {
    let src = TempDir::new("grow-ram");
    std::fs::write(src.path().join("a.txt"), b"hello").expect("write sample");
    let drive = ScriptedDrive::new(CurrentProfile::DVD_RAM, 0b11);

    let error = run_grow(drive, src.path()).expect_err("random-writable media cannot grow");
    assert!(
        matches!(error, BurnError::NativeGap(NativeGap::GrowthOnRewritable)),
        "{error:?}"
    );
}

#[test]
fn grow_refuses_a_finalized_disc() {
    let src = TempDir::new("grow-final");
    std::fs::write(src.path().join("a.txt"), b"hello").expect("write sample");
    let drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b10);

    let error = run_grow(drive, src.path()).expect_err("finalized discs cannot grow");
    assert!(
        matches!(error, BurnError::NativeGap(NativeGap::FinalizedDisc)),
        "{error:?}"
    );
}

#[test]
fn grow_gate_reports_not_enough_space_before_writing() {
    let src = TempDir::new("grow-small");
    std::fs::write(src.path().join("a.txt"), b"hello").expect("write sample");
    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b00);
    drive.free_blocks = 100;
    let log = Rc::clone(&drive.log);
    let written = Rc::clone(&drive.written);

    let mut mmc = MmcDevice::new(Box::new(drive));
    let error = grow_with_device(
        &mut mmc,
        &grow_job(src.path()),
        &mut |_| {},
        &CancelToken::default(),
    )
    .expect_err("100 blocks cannot hold a session");
    assert!(
        matches!(error, BurnError::NotEnoughSpace { free, .. } if free == 100 * 2048),
        "{error:?}"
    );
    assert!(
        !opcodes(&log.borrow()).contains(&0x2A),
        "拒绝发生在任何写入之前"
    );
    assert!(written.borrow().is_empty());
}

#[test]
fn grow_skips_the_gate_without_capacity_figures() {
    let src = TempDir::new("grow-nofigure");
    std::fs::write(src.path().join("a.txt"), b"hello").expect("write sample");
    let mut drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b00);
    drive.free_blocks = 0; // 驱动器报不出剩余块数，描述符区也全是零

    let (_, written) = run_grow(drive, src.path()).expect("grow proceeds without figures");
    assert!(read_session(written, 0).root.files.len() == 1);
}

#[test]
fn grow_speed_option_is_refused() {
    let src = TempDir::new("grow-speed");
    std::fs::write(src.path().join("a.txt"), b"hello").expect("write sample");
    let drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b00);
    let mut mmc = MmcDevice::new(Box::new(drive));
    let mut job = grow_job(src.path());
    job.speed = Some(8);

    let error = grow_with_device(&mut mmc, &job, &mut |_| {}, &CancelToken::default())
        .expect_err("speed is not implemented");
    assert!(
        matches!(error, BurnError::NativeGap(NativeGap::WriteSpeed)),
        "{error:?}"
    );
}

#[test]
fn grow_size_matches_the_planned_session() {
    let src = TempDir::new("grow-size");
    std::fs::write(src.path().join("a.txt"), b"hello").expect("write sample");
    let drive = ScriptedDrive::new(CurrentProfile::CD_R, 0b00);
    let mut mmc = MmcDevice::new(Box::new(drive));

    let size = grow_size_with_device(&mut mmc, &grow_job(src.path())).expect("size");
    let plan = crate::grow::plan_session(None, src.path(), "GROWTV1".into(), 0)
        .expect("plan the same session");
    assert_eq!(size, plan.total_bytes());
}
