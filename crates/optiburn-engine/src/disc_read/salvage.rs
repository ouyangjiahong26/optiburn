//! 未关闭轨道（刻录中断留下的那一次写入）的抢救：定位写入边界、按边界给文件定性，
//! 把能读的部分抽到本地，半截的 zip 重建为只含完整条目的包。设计依据与实测数据见
//! ADR-0022 补记。

use std::fs::File;
use std::io::Write;
use std::path::Path;

use optiburn_mmc::{DiscStatus, MmcDevice, MmcError, SECTOR_BYTES, wait_until_ready};

use super::{BlockRead, SourceKind, classify_source, zip_recover};

use crate::readback::safe_relative_path;
use crate::{BurnError, CancelToken, SalvageReport, SalvageState, SalvagedFile};

/// 未关闭轨道里已经写下去的边界（不含）：返回第一个读不出来的块，也就是“最后
/// 一个已写块加一”。二分定位，读次数是块数的对数。
///
/// “读得出来”＝“写下去了”这条判断有实测依据（2026-10-11，一张被中断的
/// DVD-RW）：未写位置回读失败，紧邻中断点报 MEDIUM ERROR（0x11），再往后报
/// LBA out of range（0x21），而已写区的每一块都读得出来。整块全零不作中断点：
/// 会话开头的系统区本来就是全零，文件内容也可以是零。
///
/// 二分假定可读性在地址上单调（写下去的是一段连续前缀）。驱动器若在已写区中间
/// 留下坏块，二分落到那块上，边界偏小、分类偏保守，且抽取时该文件会按实际读到的
/// 字节截短。空盘上轨道起点没有有效 vss 时由调用方先拦。
pub(crate) fn written_boundary(
    read: &mut BlockRead<'_>,
    start: u32,
    vss_blocks: u32,
    cancel: &CancelToken,
) -> Result<u32, BurnError> {
    let end = start.saturating_add(vss_blocks);
    let mut block = [0u8; SECTOR_BYTES];
    // 起点读不出来说明这段没有东西可救，边界就是起点。
    if !readable_block(read, start, &mut block, cancel)? {
        return Ok(start);
    }
    // 不变量：[start, low) 都读得出来，[high, end) 尚未探明。结果落在 low 上。
    let mut low = start + 1;
    let mut high = end;
    while low < high {
        let mid = low + (high - low) / 2;
        if readable_block(read, mid, &mut block, cancel)? {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    Ok(low)
}

/// 读一块，只关心读得出来还是读不出来（取消原样上报）。
fn readable_block(
    read: &mut BlockRead<'_>,
    lba: u32,
    block: &mut [u8],
    cancel: &CancelToken,
) -> Result<bool, BurnError> {
    if cancel.is_cancelled() {
        return Err(BurnError::Cancelled);
    }
    Ok(read(lba, block).is_ok())
}

/// 按写入边界给一条文件定性：数据整段在边界之前是完整，起点越界是一个字节都没写，
/// 跨过边界是半截（能读多少字节算多少）。
pub(crate) fn classify_salvage_state(extent: u32, size: u64, boundary: u32) -> SalvageState {
    if size == 0 {
        return SalvageState::Complete;
    }
    let blocks = (size as u32).div_ceil(SECTOR_BYTES as u32);
    if extent >= boundary {
        SalvageState::Missing
    } else if extent.saturating_add(blocks) <= boundary {
        SalvageState::Complete
    } else {
        SalvageState::Truncated {
            readable_bytes: u64::from(boundary - extent) * SECTOR_BYTES as u64,
        }
    }
}

/// 抢救入口：镜像文件没有未关闭轨道的概念，返回 `None`；设备路径按原生读侧打开
/// 后交给 [`salvage_unclosed_session_on_device`]。
pub(super) fn salvage_unclosed_session(
    source: &str,
    dest: &Path,
    cancel: &CancelToken,
) -> Result<Option<SalvageReport>, BurnError> {
    let SourceKind::Device(device) = classify_source(Path::new(source)) else {
        return Ok(None);
    };
    let transport =
        optiburn_transport::open(&device).map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?;
    salvage_unclosed_session_on_device(MmcDevice::new(transport), dest, cancel)
}

/// 抢救主体（[`crate::salvage`] 的原生实现）：在已打开的设备上定位未关闭轨道、探
/// 写入边界、按边界给每个文件定性、把能读的部分抽到 `dest`。
///
/// 这种轨道不在 TOC 里（驱动器只把已关闭的轨道登记进 TOC），起点由
/// `READ TRACK INFORMATION` 的未关闭轨道报出来。实测（2026-10-11）中断的写入可能把
/// 描述符与目录树完整写下去，只是没关轨道与区段，此时它的树可以读，文件数据则只
/// 有中断点之前的部分。
///
/// 普通读路径与追加门禁不用这个视图（标准读取器挂载的是最后一个已关闭区段，用
/// 未关闭的轨道会看到残缺的树），只有显式抢救才走这里。
///
/// 目录树用增长模式那套盘级地址读法（[`crate::grow::read_old_session`]），因此
/// 相对与绝对两种区段约定都能处理。
pub(super) fn salvage_unclosed_session_on_device(
    mut mmc: MmcDevice,
    dest: &Path,
    cancel: &CancelToken,
) -> Result<Option<SalvageReport>, BurnError> {
    let Some((start, vss)) = open_unclosed_track(&mut mmc)? else {
        return Ok(None);
    };
    let tree = {
        let mut read = |lba: u32, out: &mut [u8]| {
            mmc.read_blocks(lba, out)?;
            Ok(())
        };
        crate::grow::read_old_session(&mut read, start, cancel)?.root
    };
    let boundary = {
        let mut read = |lba: u32, out: &mut [u8]| {
            mmc.read_blocks(lba, out)?;
            Ok(())
        };
        written_boundary(&mut read, start, vss, cancel)?
    };
    let mut files = walk_and_extract(&mut mmc, tree, dest, boundary, cancel)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Some(SalvageReport {
        session_start: start,
        boundary,
        files,
    }))
}

/// 确认盘上确有可抢救的未关闭轨道，返回轨道起点与卷空间大小（PVD 的偏移 80..84）。
/// 没有可抢救对象时返回 `None`：空盘、没有区段信息、未关闭轨道不晚于末个已关闭
/// 区段、描述符区没落盘（读不出来）、描述符不是 ISO 9660。
fn open_unclosed_track(mmc: &mut MmcDevice) -> Result<Option<(u32, u32)>, BurnError> {
    wait_until_ready(mmc)?;
    let info = mmc.read_disc_information()?;
    if info.status == DiscStatus::Empty {
        return Ok(None);
    }
    let Some(session) = mmc.read_toc_session_info()? else {
        return Ok(None);
    };
    let track = mmc.read_track_information(crate::native::LAST_TRACK)?;
    // 未关闭轨道的起点必须晚于末个已关闭区段，否则盘上没有未关闭轨道。
    if track.start_lba <= session.last_session_start {
        return Ok(None);
    }
    let start = track.start_lba;
    let mut pvd = vec![0u8; SECTOR_BYTES];
    // 驱动器可能登记了未关闭轨道但一个块都没写下去：描述符区读失败说明没有可
    // 列出的目录树，与不是 CD001 同属“没有可抢救内容”，不把预期内的探测失败
    // 报成用户可见错误（issue #51）。
    if mmc
        .read_blocks(start + crate::grow::DESC_START, &mut pvd)
        .is_err()
    {
        return Ok(None);
    }
    if pvd[1..6] != *b"CD001" {
        return Ok(None);
    }
    let vss = u32::from_le_bytes([pvd[80], pvd[81], pvd[82], pvd[83]]);
    Ok(Some((start, vss)))
}

/// 遍历目录树，按边界给每个文件定性并把能读的部分抽出去（目录按原样建出来）。
fn walk_and_extract(
    mmc: &mut MmcDevice,
    tree: crate::grow::DirNode,
    dest: &Path,
    boundary: u32,
    cancel: &CancelToken,
) -> Result<Vec<SalvagedFile>, BurnError> {
    let mut files = Vec::new();
    let mut stack = vec![(String::new(), tree)];
    while let Some((prefix, node)) = stack.pop() {
        for file in node.files {
            let path = format!("{prefix}/{}", file.name);
            files.push(salvage_one_file(mmc, &path, &file, dest, boundary, cancel)?);
        }
        for dir in node.dirs {
            stack.push((format!("{prefix}/{}", dir.name), dir));
        }
    }
    Ok(files)
}

/// 一条文件的全部处置：定性、抽取、半截 zip 重建。抽完按实际读到的字节定最终
/// 状态，中途读失败会让文件比边界允许的更短。
fn salvage_one_file(
    mmc: &mut MmcDevice,
    path: &str,
    file: &crate::grow::OldFile,
    dest: &Path,
    boundary: u32,
    cancel: &CancelToken,
) -> Result<SalvagedFile, BurnError> {
    let planned = classify_salvage_state(file.extent, file.size, boundary);
    let mut extracted = 0u64;
    if planned != SalvageState::Missing {
        let readable = match planned {
            SalvageState::Complete => file.size,
            SalvageState::Truncated { readable_bytes } => readable_bytes.min(file.size),
            SalvageState::Missing => 0,
        };
        let target = dest.join(safe_relative_path(path)?);
        extracted = extract_salvage_file(mmc, file.extent, readable, &target, cancel)?;
    }
    let state = final_salvage_state(extracted, file.size);
    // 半截的 zip 重建一份只含完整条目的：中央目录在文件尾，缺失后常规工具
    // 打不开。重建后能直接解。只对真的写了一部分的文件做，一个字节都没写
    // 的文件在 dest 下根本不存在（也就没有原始字节可留）。重建失败或没有
    // 可保留的条目时文件不动。
    let zip = if matches!(state, SalvageState::Truncated { .. }) && is_zip_name(&file.name) {
        let target = dest.join(safe_relative_path(path)?);
        zip_recover::recover_truncated_zip(&target)?
    } else {
        None
    };
    Ok(SalvagedFile {
        path: path.to_string(),
        size: file.size,
        state,
        zip,
    })
}

/// 把一条文件能读出来的字节抽到本地：按 32 块分块读，末尾按 `readable_bytes` 截断，
/// 返回真正写下去的字节数。
///
/// 中途读失败（损坏或中断点比扫描结果更靠前）不报错：停在那里返回已写下的部分，
/// 由调用方按实际字节数定性。
pub(super) fn extract_salvage_file(
    mmc: &mut MmcDevice,
    extent: u32,
    readable_bytes: u64,
    target: &Path,
    cancel: &CancelToken,
) -> Result<u64, BurnError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = File::create(target)?;
    let mut written = 0u64;
    let mut remaining = readable_bytes;
    let mut buffer = vec![0u8; SECTOR_BYTES * optiburn_mmc::MAX_READ_BLOCKS];
    let mut lba = extent;
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let chunk_bytes = (remaining as usize).min(buffer.len());
        let blocks = chunk_bytes.div_ceil(SECTOR_BYTES);
        if mmc
            .read_blocks(lba, &mut buffer[..blocks * SECTOR_BYTES])
            .is_err()
        {
            break;
        }
        out.write_all(&buffer[..chunk_bytes])?;
        written += chunk_bytes as u64;
        remaining -= chunk_bytes as u64;
        lba += blocks as u32;
    }
    // 首块就读失败时一个字节都没写：把空壳删掉，与“没写的文件在 dest 下根本
    // 不存在”的口径一致。零长度文件 readable_bytes 为 0，不进这个分支，抽出的
    // 0 字节就是它的全部内容。
    if readable_bytes > 0 && written == 0 {
        drop(out);
        std::fs::remove_file(target)?;
    }
    Ok(written)
}
/// 按实际写下的字节数给文件定性：够了算完整（零长度文件抽出 0 字节也算完整），
/// 一个字节都没读出来且文件本来非空算没写，其余算半截。中途读失败会让实际字节数
/// 比边界允许的更短，这里以实际为准。
pub(super) fn final_salvage_state(extracted: u64, size: u64) -> SalvageState {
    if extracted >= size {
        SalvageState::Complete
    } else if extracted == 0 {
        SalvageState::Missing
    } else {
        SalvageState::Truncated {
            readable_bytes: extracted,
        }
    }
}

/// 名字是不是 zip（重建只对 zip 有意义，`.ZIP` 也认）。按字节比较：按字符切后缀
/// 在多字节名字（中文名）上会踩到非字符边界，`str` 索引直接 panic。
pub(super) fn is_zip_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() > 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".zip")
}
