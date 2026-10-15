//! 写前容量门禁的区段开销余量（ADR-0019 决策 3 的修订）。
//!
//! 待写入量（[`crate::grow_size`] 的预演字节数或镜像大小）只算数据区，不含区段
//! 的 lead-in/lead-out 与链接区。余量按介质与动作分档：CD 有免费的规范值可用
//! （ECMA-394 §5.7.1 的导出区与 libburn 的会话 lead-in），非 CD 没有，沿用改动前
//! 的 16 MiB。读得到盘上相邻区段的实测差额时优先用实测值，量不出再退回默认值。
//!
//! 实测路径只读：READ TOC Format 1 取末区段起点，READ TRACK INFORMATION 取下一
//! 个可写地址，READ(10) 读末区段的卷描述符区。任何一步读不到就退回默认值，整条路
//! 不下发任何写或修复命令。

use optiburn_mmc::{DiscStatus, MediaKind, MmcDevice, MmcError, SECTOR_BYTES};

use crate::BurnError;

/// CD 区段保持可追加时不低于这个余量（块）。首区段是 4500 块 lead-in 加 6750 块
/// lead-out，非首区段是 4500 加 2250（ECMA-394 §5.7.1 的首会话导出区 6750 块、
/// 后续区段 2250 块，libburn 的会话 lead-in 取 4500 块）。
const CD_APPEND_FIRST_FLOOR_BLOCKS: u64 = 11_250;
const CD_APPEND_FLOOR_BLOCKS: u64 = 6_750;

/// 盘上读不到实测差额时 CD 保持可追加的默认余量（块）。2026-10-15 在本机
/// （HL-DT-ST DVDRAM GP70N 加 CD-R 80min，盘上 10 个区段）量出的相邻区段差值：
/// 首区段到第二区段 11845 块，其余区段 7335 到 7346 块，取其中偏保守的一档。
const CD_APPEND_FIRST_DEFAULT_BLOCKS: u64 = 11_845;
const CD_APPEND_DEFAULT_BLOCKS: u64 = 7_335;

/// CD 封盘那一次要写的 lead-out（ECMA-394 §5.7.1）：首区段 6750 块，
/// 后续区段 2250 块。
const CD_CLOSE_FIRST_BLOCKS: u64 = 6_750;
const CD_CLOSE_BLOCKS: u64 = 2_250;

/// 非 CD 介质读不到实测差额时的默认余量：BD 的 zone 尺寸没有免费的规范值，本机也
/// 没有 DVD/BD 介质可实测，沿用改动前的 16 MiB。这是已知缺口，有对应盘片时应像
/// CD 一样改成实测加兜底。
const NON_CD_DEFAULT_BYTES: u64 = 16 * 1024 * 1024;

/// 会话内卷描述符（PVD）相对区段起点的块偏移：ISO 9660 的卷描述符区从第 16 块起。
pub(crate) const PVD_BLOCK_OFFSET: u32 = 16;

/// 写前容量门禁的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityGate {
    /// 驱动器报不出可用容量（例如可覆写介质只给格式化容量），门禁放行，由调用方
    /// 决定打提示还是静默跳过。
    Unknown,
    /// 待写入量加区段开销余量放得下。
    Fits,
}

/// 写前容量门禁：打开设备，读 Profile 与盘片状态，按动作算区段开销余量，再与可用
/// 容量比较（ADR-0019）。
///
/// `needed` 是待写入的字节数（[`crate::grow_size`] 的预演值或镜像大小），
/// `closing` 表示这次写完是否封盘（封盘只写 lead-out，不封盘还要留出下次的
/// lead-in）。放不下返回 [`BurnError::NotEnoughSpace`]；可用容量算不出来时返回
/// [`CapacityGate::Unknown`]，交给调用方放行。
pub fn capacity_gate(device: &str, needed: u64, closing: bool) -> Result<CapacityGate, BurnError> {
    let transport =
        optiburn_transport::open(device).map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?;
    let mut mmc = MmcDevice::new(transport);
    // 盘片信息或 Profile 读不到时不判：门禁是尽力而为的预检，不该成为新的故障点，
    // 真写不下由引擎写入失败兜底（ADR-0019）。
    let Ok(info) = mmc.read_disc_information() else {
        return Ok(CapacityGate::Unknown);
    };
    let Ok(profile) = mmc.get_configuration() else {
        return Ok(CapacityGate::Unknown);
    };
    // Profile 认不出来时不判余量：调用方可能改用别的引擎（CLI 的 xorriso），门禁
    // 放行，真写不下由引擎兜底。
    let Some(kind) = profile.media_kind() else {
        return Ok(CapacityGate::Unknown);
    };
    let Some(free) = mmc.read_disc_capacity().free else {
        return Ok(CapacityGate::Unknown);
    };
    let headroom = session_headroom(&mut mmc, kind, info.status, closing);
    if needed.saturating_add(headroom) > free {
        return Err(BurnError::NotEnoughSpace {
            needed,
            headroom,
            free,
        });
    }
    Ok(CapacityGate::Fits)
}

/// 一次写入要预留的区段开销余量（字节）。引擎内与调用方的写前门禁共用这一份口径。
///
/// 盘片状态为空表示要在空盘上写首区段，其余按非首区段处理。量不到实测差额时 CD
/// 取本机默认值，非 CD 取 [`NON_CD_DEFAULT_BYTES`]。
pub(crate) fn session_headroom(
    mmc: &mut MmcDevice,
    kind: MediaKind,
    status: DiscStatus,
    closing: bool,
) -> u64 {
    let first_session = status == DiscStatus::Empty;
    let blocks = match kind {
        MediaKind::Cd => {
            if closing {
                if first_session {
                    CD_CLOSE_FIRST_BLOCKS
                } else {
                    CD_CLOSE_BLOCKS
                }
            } else if first_session {
                // 空盘上不存在上一区段，实测差额无从谈起，直接取首区段默认值。
                CD_APPEND_FIRST_DEFAULT_BLOCKS.max(CD_APPEND_FIRST_FLOOR_BLOCKS)
            } else {
                let measured = measured_gap_blocks(mmc).unwrap_or(CD_APPEND_DEFAULT_BLOCKS);
                measured.max(CD_APPEND_FLOOR_BLOCKS)
            }
        }
        MediaKind::DvdMinus | MediaKind::PlusOrBdR => {
            measured_gap_blocks(mmc).unwrap_or(NON_CD_DEFAULT_BYTES / SECTOR_BYTES as u64)
        }
        // 随机可写介质（DVD-RAM、BD-RE）按块覆写，没有区段的 lead-in/lead-out。
        MediaKind::RandomWritable => 0,
    };
    blocks * SECTOR_BYTES as u64
}

/// 盘上相邻区段之间的实测差额（块）：下一个可写地址减去上一区段数据块的结尾。
///
/// 上一区段的数据块数取它自己 PVD 的卷空间大小（PVD 在区段起点加 16 块，字节
/// 80-83 小端、84-87 大端，两半必须相等且非零，标准标识是 `CD001`）。要求
/// NWA_V 置位且下一个可写地址落在末区段起点之后，任何一步读不到返回 `None`。
fn measured_gap_blocks(mmc: &mut MmcDevice) -> Option<u64> {
    let session = mmc.read_toc_session_info().ok().flatten()?;
    let track = mmc.read_track_information(crate::native::LAST_TRACK).ok()?;
    if !track.nwa_valid || track.next_writable_address <= session.last_session_start {
        return None;
    }
    let pvd_lba = session.last_session_start.checked_add(PVD_BLOCK_OFFSET)?;
    let mut block = [0u8; SECTOR_BYTES];
    mmc.read_blocks(pvd_lba, &mut block).ok()?;
    let data_blocks = parse_pvd_volume_space(&block)?;
    let data_end = session.last_session_start.checked_add(data_blocks)?;
    Some(u64::from(
        track.next_writable_address.checked_sub(data_end)?,
    ))
}

/// 从卷描述符块取卷空间大小（块）。类型字节为 1、标准标识 `CD001`、版本字节为 1，
/// 卷空间大小的双端序两个半值必须相等且非零，否则这一块不是可用的 PVD。
/// 真机诊断（`inspect_session_gaps`）也用它，好让打印出的差额与门禁同源。
pub(crate) fn parse_pvd_volume_space(block: &[u8]) -> Option<u32> {
    if block.len() < 88 || block[0] != 1 || &block[1..6] != b"CD001" || block[6] != 1 {
        return None;
    }
    let little = u32::from_le_bytes(block[80..84].try_into().ok()?);
    let big = u32::from_be_bytes(block[84..88].try_into().ok()?);
    (little != 0 && little == big).then_some(little)
}
