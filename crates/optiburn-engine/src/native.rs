//! 原生 MMC 写引擎：自己发 MMC 命令把镜像写到盘上，不依赖外部程序（ADR-0017）。
//!
//! 写序列按当前 Profile 分组（[`MediaKind`]）：顺序介质（CD-R、DVD-R 族）先设写
//! 参数页、预留轨道，再写数据；随机可写与 +R 族（DVD-RAM、BD-RE、DVD+R[W]）不设
//! 写参数、不预留，写完按需关区段。介质族之外的 Profile 一律拒绝，不猜写序列。
//!
//! 已知缺口（ADR-0017 记录）：只写空白与随机可写介质，可追加盘需要增长模式合并
//! 既有区段，尚未实现；倍速参数尚未支持（需要 SET CD SPEED/STREAMING，未在真机
//! 上核对过单位）；关区段之后的封盘由 MODE SELECT 的 multi 位决定，与 xorriso 的
//! `--close-disc` 同义。

use std::fs::File;
use std::io::Read;
use std::time::Duration;

use optiburn_mmc::{
    DiscStatus, MediaKind, MmcDevice, MmcError, SECTOR_BYTES, WriteBlock, approve_write,
    wait_until_ready, wait_until_ready_for,
};

use crate::{BurnEngine, BurnError, BurnJob, CancelToken, NativeGap};

/// 单条 WRITE(10) 携带的块数：32 块（64 KiB）。同步写、没有缓冲队列，
/// 块小一点让取消与进度的粒度都细一些，慢速介质也来得及落盘。
const CHUNK_BLOCKS: usize = 32;

/// 关区段之后驱动器还要忙一阵（写 lead-out、更新 TOC），给它的就绪时限。
const CLOSE_SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// 原生引擎：接在 [`BurnEngine`] 同一个接缝上，把 xorriso 换成自己发的 MMC 命令。
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeEngine;

impl BurnEngine for NativeEngine {
    fn name(&self) -> &'static str {
        "native"
    }

    fn burn(
        &self,
        job: &BurnJob,
        progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        // 打开失败按 MMC 报错处理：对用户来说与命令失败同类（设备不可用）。
        let transport = optiburn_transport::open(&job.device)
            .map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?;
        let mut mmc = MmcDevice::new(transport);
        burn_with_device(&mut mmc, job, progress, cancel)
    }
}

/// 写盘主体：从一台已打开的设备开始，方便测试喂替身传输层跑完整个序列。
pub(crate) fn burn_with_device(
    mmc: &mut MmcDevice,
    job: &BurnJob,
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    if job.speed.is_some() {
        return Err(BurnError::NativeGap(NativeGap::WriteSpeed));
    }
    let (kind, status) = preflight(mmc)?;
    let blocks = image_blocks(job)?;
    let start = start_lba(mmc, status)?;
    guard_capacity(mmc, kind, start, blocks)?;

    mmc.set_write_parameters(kind, job.multi)?;
    // 不预留轨道：libburn 只在 DVD-R[W] 的 DAO 与 DVD+R 的 SAO 上发 RESERVE TRACK，
    // CD 的 TAO 与 DVD 的增量写都不发。实测（2026-10-10，可追加 CD-R）照搬预留会被
    // 驱动器以 ILLEGAL REQUEST/INVALID FIELD IN CDB 拒绝。DAO 路径要用时在 mmc 里
    // 已经有现成的命令。
    write_image(mmc, job, blocks, start, progress, cancel)?;

    mmc.synchronize_cache()?;
    if kind.needs_close_session() {
        mmc.close_session()?;
        wait_until_ready_for(mmc, CLOSE_SETTLE_DEADLINE)?;
    }
    progress(1.0);
    Ok(())
}

/// 写前检查：介质就绪、盘片可写、Profile 在支持列表里。返回介质族与盘片状态
/// （状态决定起始地址，见 [`start_lba`]）。
fn preflight(mmc: &mut MmcDevice) -> Result<(MediaKind, DiscStatus), BurnError> {
    wait_until_ready(mmc)?;
    let info = mmc.read_disc_information()?;
    // 已封口是物理上写不了，引擎自己拦。可追加盘不在引擎层拒绝：门禁在调用方
    // （ADR-0006/0010，镜像写入遮住旧区段的那道），引擎按 NWA 往后写新区段，
    // 不覆写任何已经写过的位置。
    approve_write(&info, true).map_err(|block| match block {
        WriteBlock::Finalized => BurnError::NativeGap(NativeGap::FinalizedDisc),
        // accept_appendable 为真时不会走到这里，留着是为了穷尽枚举。
        WriteBlock::NeedGrowMode => BurnError::NativeGap(NativeGap::FinalizedDisc),
    })?;
    let profile = mmc.get_configuration()?;
    let kind = profile
        .media_kind()
        .ok_or(BurnError::NativeGap(NativeGap::UnsupportedProfile(
            profile.0,
        )))?;
    Ok((kind, info.status))
}

/// 写入的起始块：空盘从 0 开始；可追加盘接在 NWA（下一个可写地址）后面写新区段；
/// 随机可写介质（DVD-RAM、BD-RE）从 0 开始顺序覆写。
///
/// NWA 必须问驱动器（READ TRACK INFORMATION），不能自己按镜像大小推算：区段之间
/// 还有 lead-in/lead-out 与链接块占用的地址。
fn start_lba(mmc: &mut MmcDevice, status: DiscStatus) -> Result<u32, BurnError> {
    match status {
        DiscStatus::Appendable => {
            let track = mmc.read_track_information(LAST_TRACK)?;
            Ok(track.next_writable_address)
        }
        _ => Ok(0),
    }
}

/// READ TRACK INFORMATION 的轨道号约定：CD 上用 0xFF 表示“当前可写的那条”
/// （libburn 对 CD 同样传 0xFF，见其 mmc_read_track_info）。
const LAST_TRACK: u32 = 0xFF;

/// 镜像的块数：向上取整到 2048 字节块，末块不足时写零补齐。
fn image_blocks(job: &BurnJob) -> Result<u64, BurnError> {
    let bytes = std::fs::metadata(&job.image)?.len();
    let blocks = bytes.div_ceil(SECTOR_BYTES as u64);
    if blocks == 0 {
        return Err(BurnError::NativeGap(NativeGap::EmptyImage));
    }
    if blocks > u64::from(u32::MAX) {
        return Err(BurnError::NativeGap(NativeGap::ImageBeyondAddressRange));
    }
    Ok(blocks)
}

/// 容量检查：驱动器报得出容量就必须放得下（可追加盘按剩余空间算）。
///
/// CD 不做这个检查：READ CAPACITY 在 CD 上报的是“最后一个区段的卷空间”，而写入
/// 用的是盘级地址（实测 2026-10-10，可追加 CD-R：NWA 264720，READ CAPACITY
/// 257819），两个地址空间混着比会误判。CD 的容量预检要等 READ TRACK INFORMATION
/// 的 free blocks 落地，当前由驱动器的写错误兜底。
fn guard_capacity(
    mmc: &mut MmcDevice,
    kind: MediaKind,
    start: u32,
    blocks: u64,
) -> Result<(), BurnError> {
    if kind == MediaKind::Cd {
        return Ok(());
    }
    if let Ok(last_lba) = mmc.read_capacity()
        && u64::from(start) + blocks > u64::from(last_lba) + 1
    {
        return Err(BurnError::NativeGap(NativeGap::ImageTooLarge));
    }
    Ok(())
}

/// 写数据：分块读镜像、整块下发，块与块之间检查取消。`start` 是第一个数据块的地址。
fn write_image(
    mmc: &mut MmcDevice,
    job: &BurnJob,
    blocks: u64,
    start: u32,
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    let mut file = File::open(&job.image)?;
    let mut buffer = vec![0u8; CHUNK_BLOCKS * SECTOR_BYTES];
    let mut lba = start;
    let mut written = 0u64;
    progress(0.0);
    loop {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let filled = fill_buffer(&mut file, &mut buffer)?;
        if filled == 0 {
            break;
        }
        let blocks_here = filled.div_ceil(SECTOR_BYTES);
        let length = blocks_here * SECTOR_BYTES;
        // 末块不足一整块时补零：WRITE(10) 只认整块。
        buffer[filled..length].fill(0);
        mmc.write_blocks(lba, &mut buffer[..length])?;
        lba += blocks_here as u32;
        written += blocks_here as u64;
        progress((written as f64 / blocks as f64).min(1.0) as f32);
    }
    Ok(())
}

/// 从镜像读满一块缓冲，返回实际读到的字节数（读到文件尾时小于缓冲长度）。
fn fill_buffer(file: &mut File, buffer: &mut [u8]) -> Result<usize, BurnError> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
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
                    // READ TRACK INFORMATION：起始地址 8-11，NWA 12-15。
                    if data.len() >= 16 {
                        data[8..12].copy_from_slice(&0x0000_1000u32.to_be_bytes());
                        data[12..16].copy_from_slice(&0x0000_2000u32.to_be_bytes());
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
}

#[cfg(test)]
mod hardware_tests {
    //! 真机测试：需要光驱，用环境变量指定设备后手动跑。
    //!
    //! - 只读侦察：`cargo test -p optiburn-engine -- --ignored inspect_real_media --nocapture`
    //! - 真机写盘：`OPTIBURN_DEVICE=D: OPTIBURN_IMAGE=x.iso cargo test -p optiburn-engine -- --ignored native_burn_real --nocapture`
    //!   写入会占用一张空白或可覆写的盘，别拿有数据的盘跑。

    use super::*;
    use optiburn_mmc::{MmcDevice, wait_until_ready};

    #[test]
    #[ignore = "needs optical drive"]
    fn inspect_real_media() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
        let transport = optiburn_transport::open(&device).expect("open device");
        let mut mmc = MmcDevice::new(transport);
        wait_until_ready(&mut mmc).expect("device ready");
        let info = mmc.read_disc_information().expect("disc information");
        let profile = mmc.get_configuration().expect("current profile");
        let capacity = mmc.read_capacity();
        println!("device: {device}");
        println!("disc: status={:?} sessions={}", info.status, info.sessions);
        println!(
            "profile: {:#06x} kind={:?}",
            profile.0,
            profile.media_kind()
        );
        println!("capacity: {capacity:?}");
    }

    #[test]
    #[ignore = "needs optical drive and a writable disc"]
    fn native_burn_real() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. /dev/sr0");
        let image = std::env::var("OPTIBURN_IMAGE").expect("set OPTIBURN_IMAGE to an .iso path");
        let mut last = 0.0f32;
        NativeEngine
            .burn(
                &BurnJob {
                    image: std::path::PathBuf::from(image),
                    device,
                    speed: None,
                    multi: false,
                },
                &mut |f| last = f,
                &CancelToken::default(),
            )
            .expect("burn failed");
        assert!((last - 1.0).abs() < 1e-6);
    }

    /// 写盘 + 读回对拍：写完把刚写的块用 READ(10) 读回来，与镜像逐字节比较。
    /// 这是在没有原生读盘能力之前能拿到的最强验证。默认不封盘（multi），
    /// 盘上已有的区段不会被覆写，新会话接在 NWA 后面。
    #[test]
    #[ignore = "needs optical drive and a writable disc"]
    fn native_burn_and_read_back_real() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
        let image = std::env::var("OPTIBURN_IMAGE").expect("set OPTIBURN_IMAGE to an .iso path");
        let expected = std::fs::read(&image).expect("read the image");
        let blocks = expected.len().div_ceil(SECTOR_BYTES);

        // 与引擎同一套起点判断：空盘 0，可追加盘 NWA。
        let start = {
            let mut mmc = MmcDevice::new(optiburn_transport::open(&device).expect("open device"));
            wait_until_ready(&mut mmc).expect("device ready");
            let info = mmc.read_disc_information().expect("disc information");
            match info.status {
                DiscStatus::Appendable => {
                    mmc.read_track_information(0xFF)
                        .expect("track information")
                        .next_writable_address
                }
                _ => 0,
            }
        };
        println!("writing {} blocks at LBA {start}", blocks);

        NativeEngine
            .burn(
                &BurnJob {
                    image: std::path::PathBuf::from(&image),
                    device: device.clone(),
                    speed: None,
                    multi: true,
                },
                &mut |_| {},
                &CancelToken::default(),
            )
            .expect("burn failed");

        let mut mmc = MmcDevice::new(optiburn_transport::open(&device).expect("open device"));
        wait_until_ready_for(&mut mmc, Duration::from_secs(60)).expect("device ready after close");
        let mut readback = vec![0u8; blocks * SECTOR_BYTES];
        mmc.read_blocks(start, &mut readback).expect("read back");
        assert_eq!(
            &readback[..expected.len()],
            &expected[..],
            "读回的字节与镜像不一致"
        );
        assert!(
            readback[expected.len()..].iter().all(|b| *b == 0),
            "末块补零"
        );
        println!("read back {} bytes, identical", readback.len());
    }
}
