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
    let (start, old) = growth_start(
        mmc,
        info.status,
        &CancelToken::default(),
        job.allow_damaged_last_session,
    )?;
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
    let (start, old) = growth_start(mmc, info.status, cancel, job.allow_damaged_last_session)?;
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
    allow_damaged_last_session: bool,
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
            let old = read_graft_source(
                mmc,
                session.last_session_start,
                cancel,
                allow_damaged_last_session,
            )?;
            Ok((start, Some(old)))
        }
        DiscStatus::Finalized => Err(BurnError::NativeGap(NativeGap::FinalizedDisc)),
        DiscStatus::Other(_) => Err(BurnError::NativeGap(NativeGap::GrowthOnRewritable)),
    }
}

/// 读出要嫁接的旧区段。末区段读得出来就是它；读不出来时按候选列表（轨道起点加
/// 末区段起点，新到旧）回退，回退前要用户确认（ADR-0022）：被跳过区段里的文件
/// 不会进新区段的目录树，等于从可见视图消失。
///
/// 全部候选都读不出来时返回**末区段**的错误，让文案与今天一致。
fn read_graft_source(
    mmc: &mut MmcDevice,
    last_session_start: u32,
    cancel: &CancelToken,
    allow_damaged_last_session: bool,
) -> Result<crate::grow::OldSession, BurnError> {
    let read_session = |mmc: &mut MmcDevice, base: u32| {
        let mut read = |lba: u32, out: &mut [u8]| {
            mmc.read_blocks(lba, out)?;
            Ok(())
        };
        crate::grow::read_old_session(&mut read, base, cancel)
    };
    let newest_error = match read_session(mmc, last_session_start) {
        Ok(old) => return Ok(old),
        Err(error) => error,
    };
    for (index, base) in crate::disc_read::session_candidates(mmc)?
        .into_iter()
        .enumerate()
    {
        if base == last_session_start {
            continue;
        }
        if let Ok(old) = read_session(mmc, base) {
            if !allow_damaged_last_session {
                return Err(BurnError::DamagedLastSession {
                    skipped: index,
                    session_start: base,
                });
            }
            return Ok(old);
        }
    }
    Err(newest_error)
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
mod tests;

#[cfg(test)]
mod hardware_tests;
