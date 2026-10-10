//! 旧区段模型与读取：把盘上末区段的 Joliet 目录树读成 [`OldSession`]（ADR-0020）。
//!
//! 读侧与 `disc_read` 同一套地址约定判定：描述符区永远在区段起点加偏移，目录记录
//! 里的 extent 按探测结果加起点或原样用。旧文件的数据块一律换算成盘级绝对地址，
//! 生成新会话时直接写回记录里，数据本身不读也不重写。

use std::collections::HashSet;
use std::io::Cursor;

use hadris_iso::joliet::JolietLevel;
use hadris_iso::sync::directory::{DirDateTime, DirectoryRecord};

use optiburn_mmc::SECTOR_BYTES;

use super::DESC_START;
use crate::disc_read::{AddressMode, BlockRead, mapped_lba, probe_address_mode};
use crate::{BurnError, CancelToken};

/// 描述符区扫描的块数上限：正常区段 16 起三四块内就有终止符，超过按坏盘处理。
const MAX_DESCRIPTOR_BLOCKS: u32 = 64;
/// 单个目录数据的字节上限：坏记录不能把内存吃光。
const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;
/// 单次读盘的块数，与 MMC 读侧的单命令上限对齐。
const MAX_DIR_BLOCKS: usize = optiburn_mmc::MAX_READ_BLOCKS;

/// 盘上末区段读出的旧目录树。
#[derive(Debug, Clone)]
pub(crate) struct OldSession {
    pub root: DirNode,
}

#[derive(Debug, Clone)]
pub(crate) struct DirNode {
    /// Joliet 名，根是空串。
    pub name: String,
    pub dirs: Vec<DirNode>,
    pub files: Vec<OldFile>,
}

#[derive(Debug, Clone)]
pub(crate) struct OldFile {
    /// Joliet 名（已去掉尾部 NUL 与 `;1` 版本后缀）。
    pub name: String,
    /// 盘级绝对块号。
    pub extent: u32,
    pub size: u64,
    pub date: DirDateTime,
}

/// 读出末区段的目录树（只读，不动盘）。`base_lba` 是区段起点。
pub(crate) fn read_old_session(
    read: &mut BlockRead<'_>,
    base_lba: u32,
    cancel: &CancelToken,
) -> Result<OldSession, BurnError> {
    let (pvd, svd, desc_len) = scan_descriptors(read, base_lba)?;
    let root_extent = u32::from_le_bytes([pvd[158], pvd[159], pvd[160], pvd[161]]);
    let mode = if probe_address_mode(read, base_lba, root_extent)? {
        AddressMode::SessionRelative
    } else {
        AddressMode::DiscAbsolute { desc_len }
    };
    let mut visited = HashSet::new();
    let root = read_dir_node(
        read,
        mode,
        base_lba,
        &svd[156..190],
        "",
        0,
        &mut visited,
        cancel,
    )?;
    Ok(OldSession { root })
}

/// 扫描述符区：返回 PVD、Joliet SVD 与描述符区块数。
fn scan_descriptors(
    read: &mut BlockRead<'_>,
    base_lba: u32,
) -> Result<([u8; SECTOR_BYTES], [u8; SECTOR_BYTES], u32), BurnError> {
    let mut pvd: Option<[u8; SECTOR_BYTES]> = None;
    let mut svd: Option<[u8; SECTOR_BYTES]> = None;
    let mut desc_len = 0u32;
    for index in 0..MAX_DESCRIPTOR_BLOCKS {
        let mut sector = [0u8; SECTOR_BYTES];
        read(base_lba + DESC_START + index, &mut sector)?;
        if sector[1..6] != *b"CD001" {
            break;
        }
        desc_len = index + 1;
        match sector[0] {
            0 => {
                return Err(unsupported("the last session contains a boot record"));
            }
            1 => {
                if pvd.is_none() {
                    pvd = Some(sector);
                }
            }
            2 => {
                if svd.is_none() {
                    let mut escape = [0u8; 32];
                    escape.copy_from_slice(&sector[88..120]);
                    if JolietLevel::from_escape_sequence(&escape).is_some() {
                        svd = Some(sector);
                    }
                }
            }
            255 => break,
            _ => {}
        }
    }
    let pvd = pvd.ok_or(BurnError::NoIsoSession)?;
    let svd = svd.ok_or_else(|| unsupported("the last session has no Joliet directory tree"))?;
    Ok((pvd, svd, desc_len))
}

/// 递归读一个目录。`record` 是指向它的目录记录（34 字节定长部分起）。
#[allow(clippy::too_many_arguments)]
fn read_dir_node(
    read: &mut BlockRead<'_>,
    mode: AddressMode,
    base_lba: u32,
    record: &[u8],
    name: &str,
    depth: usize,
    visited: &mut HashSet<u32>,
    cancel: &CancelToken,
) -> Result<DirNode, BurnError> {
    let (extent, size) = parse_dir_record(record)?;
    if !size.is_multiple_of(SECTOR_BYTES as u32) {
        return Err(read_failed(
            "the last session has a directory whose size is not a block multiple",
        ));
    }
    if u64::from(size) > MAX_DIR_BYTES {
        return Err(read_failed(
            "the last session has a directory larger than 64 MiB",
        ));
    }
    if !visited.insert(extent) {
        return Err(unsupported(
            "the last session's directory tree contains a cycle",
        ));
    }
    // extent 来自盘上字节，坏盘可以写一个贴近 32 位上限的值。起点先按约定算，相对
    // 约定用 checked_add（`mapped_lba` 在那里是 wrapping_add，先算会回绕成盘内小
    // LBA，后面的长度检查就再也发现不了），长度再补一次检查。不查的话 dev 构建
    // panic，release 构建按错误 LBA 读，把目录读成空目录再嫁接进新区段。
    let start = match mode {
        AddressMode::SessionRelative => base_lba.checked_add(extent).ok_or_else(|| {
            read_failed("the last session has a directory beyond the addressable blocks")
        })?,
        AddressMode::DiscAbsolute { .. } => mapped_lba(mode, base_lba, extent),
    };
    start
        .checked_add(size.div_ceil(SECTOR_BYTES as u32))
        .ok_or_else(|| {
            read_failed("the last session has a directory beyond the addressable blocks")
        })?;
    let mut data = vec![0u8; size as usize];
    read_blocks(read, start, &mut data, cancel)?;

    let mut node = DirNode {
        name: name.to_string(),
        dirs: Vec::new(),
        files: Vec::new(),
    };
    let mut offset = 0usize;
    while offset < data.len() {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let len = data[offset] as usize;
        if len == 0 {
            // 记录之间的补零：跳到下一块的开头（hadris 的读侧同一规则）。
            offset = (offset / SECTOR_BYTES + 1) * SECTOR_BYTES;
            continue;
        }
        if len < 34 || offset % SECTOR_BYTES + len > SECTOR_BYTES || offset + len > data.len() {
            return Err(read_failed(
                "the last session has an invalid directory record",
            ));
        }
        let mut cursor = Cursor::new(&data[offset..offset + len]);
        let parsed = DirectoryRecord::parse(&mut cursor).map_err(|e| {
            read_failed(&format!(
                "the last session has an invalid directory record: {e}"
            ))
        })?;
        offset += len;
        if is_self_reference(parsed.name()) {
            continue;
        }
        let entry_name = normalize_name(parsed.joliet_name());
        if entry_name.is_empty() {
            return Err(read_failed(
                "the last session has an entry with an empty name",
            ));
        }
        if parsed.is_directory() {
            if depth + 1 > super::MAX_DEPTH {
                return Err(unsupported("directory nesting deeper than 8 levels"));
            }
            let child = read_dir_node(
                read,
                mode,
                base_lba,
                &data[offset - len..offset],
                &entry_name,
                depth + 1,
                visited,
                cancel,
            )?;
            node.dirs.push(child);
        } else {
            let header = parsed.header();
            if header.flags & 0x80 != 0 {
                return Err(unsupported(&format!(
                    "the old file {} spans multiple extents",
                    super::child_path(name, &entry_name)
                )));
            }
            if header.extent.read() == 0 && header.data_len.read() > 0 {
                return Err(read_failed(
                    "the last session has a non-empty file at a null extent",
                ));
            }
            if has_symlink_susp(parsed.system_use()) {
                return Err(unsupported(&format!(
                    "the old file {} is a symbolic link",
                    super::child_path(name, &entry_name)
                )));
            }
            node.files.push(OldFile {
                name: entry_name,
                extent: mapped_lba(mode, base_lba, header.extent.read()),
                size: u64::from(header.data_len.read()),
                date: header.date_time,
            });
        }
    }
    Ok(node)
}

/// 目录记录的定长部分：extent 与数据长度。
fn parse_dir_record(record: &[u8]) -> Result<(u32, u32), BurnError> {
    let mut cursor = Cursor::new(record);
    let parsed = DirectoryRecord::parse(&mut cursor).map_err(|e| {
        read_failed(&format!(
            "the last session has an invalid directory record: {e}"
        ))
    })?;
    let header = parsed.header();
    Ok((header.extent.read(), header.data_len.read()))
}

/// 从盘上读满一段目录数据，按块拆分命令。
fn read_blocks(
    read: &mut BlockRead<'_>,
    start: u32,
    out: &mut [u8],
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    let mut done = 0usize;
    while done < out.len() {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let blocks = ((out.len() - done) / SECTOR_BYTES).min(MAX_DIR_BLOCKS);
        let end = done + blocks * SECTOR_BYTES;
        read(start + (done / SECTOR_BYTES) as u32, &mut out[done..end])?;
        done = end;
    }
    Ok(())
}

/// “.”与“..”记录：名字是单个字节 0 或 1。必须限定长度，Joliet 名是 UTF-16BE，
/// ASCII 名的首字节也是 0（“a”编码为 `00 61`），不限长度会把这类条目整个吞掉。
fn is_self_reference(name: &[u8]) -> bool {
    name.len() == 1 && matches!(name[0], 0 | 1)
}

/// 记录的 SUSP/RRIP 系统用区里是否有符号链接条目（`SL`）。旧区段的 RRIP 信息不结转
/// （ADR-0020），把符号链接当普通文件保留是静默降级，这里直接拒绝。
fn has_symlink_susp(system_use: &[u8]) -> bool {
    let mut offset = 0;
    while offset + 4 <= system_use.len() {
        let length = system_use[offset + 2] as usize;
        if length < 4 || offset + length > system_use.len() {
            break;
        }
        if &system_use[offset..offset + 2] == b"SL" {
            return true;
        }
        offset += length;
    }
    false
}

/// Joliet 名归一：去尾部 NUL 填充与 `;1` 版本后缀。
fn normalize_name(mut name: String) -> String {
    while name.ends_with('\0') {
        name.pop();
    }
    if let Some((head, version)) = name.rsplit_once(';')
        && !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
    {
        name = head.to_string();
    }
    name
}

fn unsupported(detail: &str) -> BurnError {
    BurnError::GrowUnsupported(detail.to_string())
}

fn read_failed(detail: &str) -> BurnError {
    BurnError::ReadFailed(detail.to_string())
}
