//! 原生 MMC 写引擎：自己发 MMC 命令把镜像写到盘上，不依赖外部程序（ADR-0017）。
//!
//! 写序列按当前 Profile 分组（[`MediaKind`]）：空盘从 LBA 0 写，可追加盘从 NWA
//! 往后写新区段，随机可写介质从 0 覆写。CD 与 DVD-R 族先设写参数页，+R 族与
//! 随机可写介质不发参数页。写完冲刷缓存，除随机可写介质外关区段。介质族之外的
//! Profile 一律拒绝，不猜写序列。
//!
//! 增长模式（[`grow`]）在可追加盘上读出旧区段的目录树，把源目录嫁接上去后生成
//! 新区段（绝对地址约定见 [`crate::grow`]，决策见 ADR-0020）。随机可写介质不
//! 支持增长：那类盘可以直接整体覆写。
//!
//! 倍速参数尚未支持（需要 SET CD SPEED/STREAMING，未在真机上核对过单位）。
//! 写失败后的重试与忙等（路线图里的 REQUEST SENSE 一路）未实现，只有写前与关区段
//! 后的就绪轮询。单条 WRITE 固定 32 块，未按介质类型调整。关区段之后的封盘由
//! MODE SELECT 的 multi 位决定，与 xorriso 的 `--close-disc` 同义。

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use optiburn_mmc::{
    DiscStatus, MediaKind, MmcDevice, MmcError, SECTOR_BYTES, wait_until_ready,
    wait_until_ready_for,
};

use crate::{BurnEngine, BurnError, BurnJob, CancelToken, GrowJob, NativeGap, SESSION_OVERHEAD};

/// 单条 WRITE(10) 携带的块数：32 块（64 KiB）。同步写、没有缓冲队列，
/// 块小一点让取消与进度的粒度都细一些，慢速介质也来得及落盘。实测过这一档
/// （CD-R 写 81 块分三条命令），更大的块与按介质调整留待有对应介质时验证。
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
    write_image(mmc, &job.image, blocks, start, progress, cancel)?;

    mmc.synchronize_cache()?;
    if kind.needs_close_session() {
        mmc.close_session()?;
        wait_until_ready_for(mmc, CLOSE_SETTLE_DEADLINE)?;
    }
    progress(1.0);
    Ok(())
}

/// 增长模式的入口：打开设备后交给 [`grow_with_device`]，与 [`NativeEngine::burn`]
/// 同一条规则处理设备打开失败。
pub(crate) fn grow(
    job: &GrowJob,
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    let transport = optiburn_transport::open(&job.device)
        .map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?;
    let mut mmc = MmcDevice::new(transport);
    grow_with_device(&mut mmc, job, progress, cancel)
}

/// 增长会话的尺寸预演：只打开设备读盘片状态与旧区段模型，算出盘上要写多少字节
/// （CLI 与 GUI 的写前容量门禁用，ADR-0019）。
pub(crate) fn grow_size(job: &GrowJob) -> Result<u64, BurnError> {
    let transport = optiburn_transport::open(&job.device)
        .map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?;
    let mut mmc = MmcDevice::new(transport);
    grow_size_with_device(&mut mmc, job)
}

/// 预演主体：从一台已打开的设备开始，与 [`grow_with_device`] 共用状态路由。
pub(crate) fn grow_size_with_device(mmc: &mut MmcDevice, job: &GrowJob) -> Result<u64, BurnError> {
    wait_until_ready(mmc)?;
    let info = mmc.read_disc_information()?;
    let (start, old) = growth_start(mmc, info.status, &CancelToken::default())?;
    let plan = crate::grow::plan_session(old, &job.src, job.volume_id.clone(), start)?;
    Ok(plan.total_bytes())
}

/// 写盘主体：从一台已打开的设备开始，方便测试喂替身传输层跑完整个序列。
pub(crate) fn grow_with_device(
    mmc: &mut MmcDevice,
    job: &GrowJob,
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    if job.speed.is_some() {
        return Err(BurnError::NativeGap(NativeGap::WriteSpeed));
    }
    wait_until_ready(mmc)?;
    let info = mmc.read_disc_information()?;
    let profile = mmc.get_configuration()?;
    let kind = profile
        .media_kind()
        .ok_or(BurnError::NativeGap(NativeGap::UnsupportedProfile(
            profile.0,
        )))?;
    let (start, old) = growth_start(mmc, info.status, cancel)?;
    let plan = crate::grow::plan_session(old, &job.src, job.volume_id.clone(), start)?;
    guard_grow_capacity(mmc, plan.total_bytes())?;

    mmc.set_write_parameters(kind, !job.close_disc)?;
    // 会话镜像先落到本地临时文件再按块写盘：生成要按源文件随机读，边生成边下发
    // 会把写命令之间的间隔拉长，慢速介质上更容易写坏。
    let staging = StagingImage::create(&plan, cancel)?;
    write_image(mmc, staging.path(), staging.blocks, start, progress, cancel)?;

    mmc.synchronize_cache()?;
    if kind.needs_close_session() {
        mmc.close_session()?;
        wait_until_ready_for(mmc, CLOSE_SETTLE_DEADLINE)?;
    }
    progress(1.0);
    Ok(())
}

/// 增长模式的写前路由：返回新区段的起点与旧区段模型。空盘从 0 起写第一区段；
/// 可追加盘接在 NWA（下一个可写地址）后面写，旧区段则从 READ TOC Format 1 报的
/// 末区段起点读（NWA 指向旧区段之后的位置，不能用来定位旧区段的内容）；封口盘与
/// 随机可写介质（DVD-RAM、BD-RE）拒绝。末区段不是 ISO 9660 时读旧区段会报
/// `NoIsoSession`，调用方门禁先拦住这类盘，这里是兜底。
fn growth_start(
    mmc: &mut MmcDevice,
    status: DiscStatus,
    cancel: &CancelToken,
) -> Result<(u32, Option<crate::grow::OldSession>), BurnError> {
    match status {
        DiscStatus::Empty => Ok((0, None)),
        DiscStatus::Appendable => {
            let start = mmc
                .read_track_information(LAST_TRACK)?
                .next_writable_address;
            let Some(session) = mmc.read_toc_session_info()? else {
                return Err(BurnError::NoIsoSession);
            };
            let old = {
                let mut read = |lba: u32, out: &mut [u8]| {
                    mmc.read_blocks(lba, out)?;
                    Ok(())
                };
                crate::grow::read_old_session(&mut read, session.last_session_start, cancel)?
            };
            Ok((start, Some(old)))
        }
        DiscStatus::Finalized => Err(BurnError::NativeGap(NativeGap::FinalizedDisc)),
        DiscStatus::Other(_) => Err(BurnError::NativeGap(NativeGap::GrowthOnRewritable)),
    }
}

/// 增长会话的容量门禁（ADR-0019 的口径）：驱动器报得出可用容量就让新会话加上
/// 区段开销必须放得下。口径与调用方的写前门禁同源（[`MmcDevice::read_disc_capacity`]），
/// 读不到时跳过，让驱动器的写错误兜底。
fn guard_grow_capacity(mmc: &mut MmcDevice, needed: u64) -> Result<(), BurnError> {
    let Some(free) = mmc.read_disc_capacity().free else {
        return Ok(());
    };
    if needed + SESSION_OVERHEAD > free {
        return Err(BurnError::NotEnoughSpace { needed, free });
    }
    Ok(())
}

/// 增长会话的临时镜像：生成完的文件在所有返回路径上都被删掉。
struct StagingImage {
    path: PathBuf,
    blocks: u64,
}

impl StagingImage {
    fn create(plan: &crate::grow::SessionPlan, cancel: &CancelToken) -> Result<Self, BurnError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("optiburn-grow-{}-{nanos}.iso", std::process::id()));
        let staging = Self {
            path,
            blocks: plan.total_bytes().div_ceil(SECTOR_BYTES as u64),
        };
        {
            let mut file = File::create(&staging.path)?;
            plan.write_image(&mut file, cancel)?;
            file.sync_all()?;
        }
        Ok(staging)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 写前检查：介质就绪、盘片可写、Profile 在支持列表里。返回介质族与盘片状态
/// （状态决定起始地址，见 [`start_lba`]）。
fn preflight(mmc: &mut MmcDevice) -> Result<(MediaKind, DiscStatus), BurnError> {
    wait_until_ready(mmc)?;
    let info = mmc.read_disc_information()?;
    // 已封口是物理上写不了，引擎自己拦。可追加盘不在引擎层拒绝：门禁在调用方
    // （ADR-0006/0010，镜像写入遮住旧区段的那道），引擎按 NWA 往后写新区段，
    // 不覆写任何已经写过的位置。
    if info.status == DiscStatus::Finalized {
        return Err(BurnError::NativeGap(NativeGap::FinalizedDisc));
    }
    let profile = mmc.get_configuration()?;
    let kind = profile
        .media_kind()
        .ok_or(BurnError::NativeGap(NativeGap::UnsupportedProfile(
            profile.0,
        )))?;
    Ok((kind, info.status))
}

/// 写入的起始块：空盘从 0 开始，可追加盘接在 NWA（下一个可写地址）后面写新区段，
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
/// 刻录（镜像文件）与增长（临时生成的会话镜像）共用这一条写序列。
fn write_image(
    mmc: &mut MmcDevice,
    image: &Path,
    blocks: u64,
    start: u32,
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    let mut file = File::open(image)?;
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
        /// READ TRACK INFORMATION 报的剩余可写块数（容量门禁用）。
        free_blocks: u32,
        /// READ TRACK INFORMATION 报的下一个可写地址（新区段起点）。
        nwa: u32,
        /// READ TOC Format 1 报的末区段起点（旧区段内容的位置）。
        last_session_start: u32,
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
                last_session_start: 0x1000,
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
                    // READ TRACK INFORMATION：起始地址 8-11，NWA 12-15，剩余块数 16-19。
                    if data.len() >= 20 {
                        data[8..12].copy_from_slice(&self.nwa.to_be_bytes());
                        data[12..16].copy_from_slice(&self.nwa.to_be_bytes());
                        data[16..20].copy_from_slice(&self.free_blocks.to_be_bytes());
                    }
                }
                0x43 => {
                    // READ TOC Format 1：数据长度 10，首末会话编号 1 与 2，
                    // 描述符的字节 4-7（响应 8-11）是末区段起始地址。
                    if data.len() >= 12 {
                        data[0] = 0;
                        data[1] = 10;
                        data[2] = 1;
                        data[3] = 2;
                        data[4] = 1;
                        data[8..12].copy_from_slice(&self.last_session_start.to_be_bytes());
                    }
                }
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
        }
    }

    /// 跑一次增长写盘，返回命令顺序与假驱动器收到的写入载荷。
    fn run_grow(
        mut drive: ScriptedDrive,
        src: &Path,
    ) -> Result<(Vec<Vec<u8>>, Vec<u8>), BurnError> {
        let log = Rc::clone(&drive.log);
        let written = Rc::clone(&drive.written);
        drive.cancel_after_first_write = None;
        let mut mmc = MmcDevice::new(Box::new(drive));
        let result = grow_with_device(
            &mut mmc,
            &grow_job(src),
            &mut |_| {},
            &CancelToken::default(),
        );
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
        grown[0x2000 * SECTOR_BYTES..0x2000 * SECTOR_BYTES + written.len()]
            .copy_from_slice(&written);
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

    /// 增长模式真机：把盘上现有路径集合记下来，追加两个小文件（其中一个中文名）
    /// 到卷标 GROWTV1，再读回对拍。默认不封盘，盘上原有区段不动。
    ///
    /// `OPTIBURN_GROW_DUMP` 给路径时另写一份合成镜像（`[区段起点块零] + 会话字节`），
    /// 供 Linux 上 `mount -t iso9660 -o loop,ro,sbsector=<起点>` 做独立判据。
    #[test]
    #[ignore = "needs optical drive and an appendable disc"]
    fn native_grow_and_read_back_real() {
        let device = std::env::var("OPTIBURN_DEVICE").expect("set OPTIBURN_DEVICE, e.g. D:");
        let before: Vec<String> = crate::list_tree(&device)
            .expect("list the disc before growing")
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        println!("盘上原有 {} 个条目：{:?}", before.len(), before);

        let src = std::env::temp_dir().join(format!("optiburn-grow-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).expect("create source dir");
        let ascii = b"optiburn native grow probe\n".to_vec();
        let chinese = "中文追加测试\n".as_bytes().to_vec();
        std::fs::write(src.join("grow-probe.txt"), &ascii).expect("write ascii file");
        std::fs::write(src.join("中文追加.txt"), &chinese).expect("write chinese file");

        let job = GrowJob {
            src: src.clone(),
            device: device.clone(),
            speed: None,
            volume_id: "GROWTV1".to_string(),
            close_disc: false,
        };
        let mut last = 0.0f32;
        grow(
            &job,
            &mut |fraction| last = fraction,
            &CancelToken::default(),
        )
        .expect("grow failed");
        assert!((last - 1.0).abs() < 1e-6, "进度收在 1.0");
        assert_eq!(
            crate::read_volume_id(&device).expect("volume id"),
            "GROWTV1"
        );

        // 读回：旧路径集合是新集合的子集，两个新文件逐字节一致。
        let after: Vec<String> = crate::list_tree(&device)
            .expect("list the disc after growing")
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        println!("追加后 {} 个条目：{:?}", after.len(), after);
        for path in &before {
            assert!(after.contains(path), "旧路径 {path} 在新会话里不见了");
        }
        let out = std::env::temp_dir().join(format!("optiburn-grow-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).expect("create output dir");
        crate::extract_paths(
            &device,
            &["/grow-probe.txt".to_string(), "/中文追加.txt".to_string()],
            &out,
            &CancelToken::default(),
        )
        .expect("extract the appended files");
        assert_eq!(std::fs::read(out.join("grow-probe.txt")).unwrap(), ascii);
        assert_eq!(std::fs::read(out.join("中文追加.txt")).unwrap(), chinese);
        let _ = std::fs::remove_dir_all(&out);
        let _ = std::fs::remove_dir_all(&src);

        if let Ok(dump) = std::env::var("OPTIBURN_GROW_DUMP") {
            dump_session(&device, Path::new(&dump)).expect("dump the session");
            println!("会话镜像已写到 {dump}");
        }
    }

    /// 把盘上末区段（起点块 + 区段字节）读成一份合成镜像：起点块用零填充，末尾
    /// 补上会话的卷空间大小。Linux 上可用 `mount -t iso9660 -o loop,ro,sbsector=`
    /// 复核绝对地址约定。
    fn dump_session(device: &str, path: &Path) -> Result<(), BurnError> {
        let mut mmc = MmcDevice::new(
            optiburn_transport::open(device).map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?,
        );
        wait_until_ready_for(&mut mmc, Duration::from_secs(60))?;
        let base = mmc
            .read_toc_session_info()?
            .ok_or_else(|| BurnError::ReadFailed("no session to dump".to_string()))?
            .last_session_start;
        let mut pvd = vec![0u8; SECTOR_BYTES];
        mmc.read_blocks(base + 16, &mut pvd)?;
        if pvd[1..6] != *b"CD001" {
            return Err(BurnError::NoIsoSession);
        }
        let blocks = u32::from_le_bytes([pvd[80], pvd[81], pvd[82], pvd[83]]);
        let mut image = vec![0u8; base as usize * SECTOR_BYTES + blocks as usize * SECTOR_BYTES];
        mmc.read_blocks(
            base,
            &mut image[base as usize * SECTOR_BYTES..base as usize * SECTOR_BYTES + SECTOR_BYTES],
        )?;
        let start = base as usize * SECTOR_BYTES;
        let mut offset = SECTOR_BYTES;
        while offset < blocks as usize * SECTOR_BYTES {
            let here = (blocks as usize * SECTOR_BYTES - offset)
                .min(optiburn_mmc::MAX_READ_BLOCKS * SECTOR_BYTES);
            let from = base + (offset / SECTOR_BYTES) as u32;
            let target = start + offset;
            mmc.read_blocks(from, &mut image[target..target + here])?;
            offset += here;
        }
        std::fs::write(path, &image)?;
        println!(
            "区段起点 {base}，卷空间 {blocks} 块，dump {} 字节",
            image.len()
        );
        Ok(())
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
