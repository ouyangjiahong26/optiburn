//! 截断 zip 的抢救：按本地文件头链走一遍，只保留完整的条目，重建中央目录。
//!
//! 中断写入留下的 zip 是「前若干个条目完整、最后一个条目截断、中央目录完全缺失」
//! 的形态。中央目录在文件尾，因此原始文件既解不开也没法用常规工具修复；这里按
//! 本地头链把完整的条目挑出来，重新写一份带中央目录与 EOCD 的 zip，让常规解压
//! 工具能直接用。
//!
//! 判定「完整」用两条：条目数据按本地头（或数据描述符）给出的长度能在可读范围
//! 内取全；条目的 CRC32 与数据对得上。第二条能挡住中断点落在数据中间、而下一个
//! 本地头又恰好出现在预期位置的情况。

use std::io::Write;
use std::path::Path;

use crate::{BurnError, ZipSalvage};

/// 本地文件头与数据描述符里出现 zip64 扩展（长度字段为 0xFFFFFFFF）时不重建：
/// 那要走 zip64 的额外字段与 EOCD64，收益不值这个复杂度，保留原始字节更实在。
const ZIP64_MARKER: u32 = 0xFFFF_FFFF;

/// 一条条目的定位结果。
struct Entry {
    /// 条目在文件里的起点（本地头签名处）。
    start: usize,
    /// 条目在文件里的终点（数据或数据描述符之后）。
    end: usize,
    /// 本地头各字段里重建中央目录要用到的部分。
    flags: u16,
    method: u16,
    time: u16,
    date: u16,
    crc: u32,
    /// 数据长度（中央目录字段）。带数据描述符的条目写在描述符里。
    compressed: u32,
    uncompressed: u32,
    /// 按本地头链算出来的实际数据跨度，用来判长度对不对得上。
    data_len: usize,
    name: Vec<u8>,
    extra_len: u16,
}

impl Entry {
    fn name_text(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }
}

/// 抢救一个截断的 zip：读 `path`，把完整的条目重建成新的 zip 写回 `path`。
///
/// 返回值有三种情形。`Ok(None)`：这不是本地头链（不像 zip）或结构本来就是全的
/// （中央目录还在），文件不动。`Ok(Some(..))` 且 `entries > 0`：文件已换成重建后
/// 的 zip。`Ok(Some(..))` 且 `entries == 0`：链是 zip 的链，但一条完整条目都没有
/// （中断点落在第一条里面），文件按原始字节保留，`dropped_entry` 报出是哪一条。
pub(crate) fn recover_truncated_zip(path: &Path) -> Result<Option<ZipSalvage>, BurnError> {
    let data = std::fs::read(path)?;
    let Some((entries, dropped)) = scan_entries(&data)? else {
        return Ok(None);
    };
    let stored_bytes = entries
        .iter()
        .map(|entry| u64::from(entry.uncompressed))
        .sum();
    // 中央目录与 EOCD 的条目数是 16 位、偏移与总长是 32 位。装不下就放弃重建，
    // 保留原始字节（与 zip64 同一取舍，不写一个自相矛盾的 EOCD）。
    let rebuilt = !entries.is_empty() && fits_plain_zip(&entries);
    if rebuilt {
        let rebuilt = build_zip(&data, &entries)?;
        let mut file = std::fs::File::create(path)?;
        file.write_all(&rebuilt)?;
    }
    Ok(Some(ZipSalvage {
        entries: entries.len() as u32,
        stored_bytes,
        dropped_entry: dropped,
        rebuilt,
    }))
}

/// 条目数与总长是否装得进 32 位 zip 的中央目录与 EOCD。
fn fits_plain_zip(entries: &[Entry]) -> bool {
    if entries.len() > u16::MAX as usize {
        return false;
    }
    // 条目的本地部分加中央目录条目（头部 46 字节加名字）。
    let total: u64 = entries
        .iter()
        .map(|entry| (entry.end - entry.start) as u64 + 46 + entry.name.len() as u64)
        .sum();
    total <= u64::from(u32::MAX)
}

/// 本地头链的扫描结果：`None` 表示不是 zip 链或结构本来就全，`Some` 给完整条目
/// 与中断点落在的那条条目的名字。
type ScanOutcome = Result<Option<(Vec<Entry>, Option<String>)>, BurnError>;

/// 走本地头链。`None` 表示不是 zip 本地头链，或结构本来就是全的（见到中央目录或
/// EOCD，说明读到的其实是完整文件）；`Some` 表示链是 zip 的链，附带完整条目与
/// 中断点落在的那个条目名。
fn scan_entries(data: &[u8]) -> ScanOutcome {
    if !data.starts_with(b"PK\x03\x04") {
        return Ok(None);
    }
    let mut entries = Vec::new();
    let mut offset = 0usize;
    loop {
        // 中央目录 / EOCD：说明这个 zip 结构其实是全的，不再重建。
        if data[offset..].starts_with(b"PK\x01\x02") || data[offset..].starts_with(b"PK\x05\x06") {
            return Ok(None);
        }
        let Some(entry) = parse_entry(data, offset)? else {
            // 链断在这里：后面这一条（如果有名字）就是没写完的那条。
            let dropped = dropped_name(data, offset);
            return Ok(Some((entries, dropped)));
        };
        if !entry_complete(data, &entry)? {
            return Ok(Some((entries, Some(entry.name_text()))));
        }
        offset = entry.end;
        entries.push(entry);
    }
}

/// 解析偏移处的一条本地头，签名不对返回 `None`（链断）。
///
/// 数据描述符（通用位标记 bit 3）的情况：长度字段是 0，真实长度在数据后面的描述符
/// 里。这里按「下一个本地头的位置倒退」定出数据长度，因此描述符含不含签名都能认。
fn parse_entry(data: &[u8], offset: usize) -> Result<Option<Entry>, BurnError> {
    let header = data.get(offset..offset + 30);
    let Some(header) = header else {
        return Ok(None);
    };
    if !header.starts_with(b"PK\x03\x04") {
        return Ok(None);
    }
    let flags = u16::from_le_bytes([header[6], header[7]]);
    let method = u16::from_le_bytes([header[8], header[9]]);
    let time = u16::from_le_bytes([header[10], header[11]]);
    let date = u16::from_le_bytes([header[12], header[13]]);
    let crc = u32::from_le_bytes([header[14], header[15], header[16], header[17]]);
    let compressed = u32::from_le_bytes([header[18], header[19], header[20], header[21]]);
    let uncompressed = u32::from_le_bytes([header[22], header[23], header[24], header[25]]);
    let name_len = u16::from_le_bytes([header[26], header[27]]) as usize;
    let extra_len = u16::from_le_bytes([header[28], header[29]]);
    let name_start = offset + 30;
    let Some(name) = data.get(name_start..name_start + name_len) else {
        return Ok(None);
    };
    let data_start = name_start + name_len + extra_len as usize;
    let has_descriptor = flags & 0b1000 != 0;
    let mut entry = Entry {
        start: offset,
        end: data_start,
        flags,
        method,
        time,
        date,
        crc,
        compressed,
        uncompressed,
        data_len: 0,
        name: name.to_vec(),
        extra_len,
    };
    if has_descriptor {
        // 数据描述符在数据之后，真实长度只信它（本地头那三个字段这时是零）。描述符
        // 本身没有签名，只能先找下一个结构（下一条本地头、中央目录或 EOCD）的签名，
        // 再往前试 16 字节（带 `PK\x07\x08`）与 12 字节两种形态。条目数据里出现同
        // 样的签名时会碰到假候选（zip 套 zip、docx、apk 这类数据里很常见），所以候选
        // 必须自洽才算数：描述符里的数据长度要与算出来的跨度一致，存储条目的 CRC32
        // 还要与数据对得上。都不自洽说明链断在这里。
        let mut candidate = find_next_structure(data, data_start);
        loop {
            let Some(next) = candidate else {
                return Ok(None);
            };
            if let Some(fields) = descriptor_at(data, data_start, next, method) {
                entry.crc = fields.0;
                entry.compressed = fields.1;
                entry.uncompressed = fields.2;
                entry.data_len = fields.3;
                entry.end = next;
                return Ok(Some(entry));
            }
            candidate = find_next_structure(data, next + 4);
        }
    }
    entry.data_len = compressed as usize;
    entry.end = data_start + entry.data_len;
    Ok(Some(entry))
}

/// 在候选位置试两种描述符形态，返回自洽的那一种的（crc、压缩长度、解压长度、数据
/// 跨度）。存储条目要求 CRC32 与数据一致（crc 为零表示没写，只核长度），压缩条目
/// 只核长度（要解压才能算 CRC，见模块头注释）。
#[allow(clippy::type_complexity)]
fn descriptor_at(
    data: &[u8],
    data_start: usize,
    next: usize,
    method: u16,
) -> Option<(u32, u32, u32, usize)> {
    for (descriptor_start, descriptor) in [
        (next.checked_sub(16)?, next - 12),
        (next.checked_sub(12)?, next - 12),
    ] {
        if descriptor_start < data_start || next - 12 < data_start {
            continue;
        }
        if descriptor_start == next - 16
            && data[descriptor_start..descriptor_start + 4] != *b"PK\x07\x08"
        {
            continue;
        }
        let crc = u32::from_le_bytes([
            data[descriptor],
            data[descriptor + 1],
            data[descriptor + 2],
            data[descriptor + 3],
        ]);
        let compressed = u32::from_le_bytes([
            data[descriptor + 4],
            data[descriptor + 5],
            data[descriptor + 6],
            data[descriptor + 7],
        ]);
        let uncompressed = u32::from_le_bytes([
            data[descriptor + 8],
            data[descriptor + 9],
            data[descriptor + 10],
            data[descriptor + 11],
        ]);
        let data_len = descriptor_start - data_start;
        if compressed as usize != data_len {
            continue;
        }
        if method == 0 && crc != 0 {
            let payload = data.get(data_start..data_start + data_len)?;
            if crc32(payload) != crc {
                continue;
            }
        }
        return Some((crc, compressed, uncompressed, data_len));
    }
    None
}

/// 从 `from` 起找下一个结构签名（下一条本地头、中央目录或 EOCD）。
fn find_next_structure(data: &[u8], from: usize) -> Option<usize> {
    if from > data.len() {
        return None;
    }
    data[from..]
        .windows(4)
        .position(|window| {
            window == b"PK\x03\x04" || window == b"PK\x01\x02" || window == b"PK\x05\x06"
        })
        .map(|offset| from + offset)
}

/// 条目是否完整：数据在可读范围里、长度字段与本地头链算出来的跨度一致，且存储
/// （未压缩）时 CRC32 与头里的值对得上。
fn entry_complete(data: &[u8], entry: &Entry) -> Result<bool, BurnError> {
    if entry.end > data.len() {
        return Ok(false);
    }
    if entry.compressed == ZIP64_MARKER || entry.uncompressed == ZIP64_MARKER {
        return Ok(false);
    }
    let data_start = entry.start + 30 + entry.name.len() + entry.extra_len as usize;
    let payload = &data[data_start..data_start + entry.data_len];
    // 压缩过的数据要解压才能核对 CRC，这里只核长度；长度对得上、数据读得出来，
    // 已经足够判断条目写没写完。
    if entry.method == 0 && entry.crc != 0 {
        return Ok(crc32(payload) == entry.crc);
    }
    Ok(true)
}

/// 链断处若还有一条本地头，取它的名字当"没保留下来"的条目名。
fn dropped_name(data: &[u8], offset: usize) -> Option<String> {
    let header = data.get(offset..offset + 30)?;
    if !header.starts_with(b"PK\x03\x04") {
        return None;
    }
    let name_len = u16::from_le_bytes([header[26], header[27]]) as usize;
    let name = data.get(offset + 30..offset + 30 + name_len)?;
    Some(String::from_utf8_lossy(name).into_owned())
}

/// 重建 zip：原样抄完整条目的字节，再补中央目录与 EOCD。
fn build_zip(data: &[u8], entries: &[Entry]) -> Result<Vec<u8>, BurnError> {
    let mut out = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for entry in entries {
        offsets.push(out.len() as u32);
        out.extend_from_slice(&data[entry.start..entry.end]);
    }
    let directory_offset = out.len() as u32;
    for (entry, offset) in entries.iter().zip(&offsets) {
        // 中央目录条目：版本、标志、方法、时间、CRC、两个长度、名字，额外字段留空。
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&20u16.to_le_bytes()); // 制作版本
        out.extend_from_slice(&20u16.to_le_bytes()); // 解压所需版本
        out.extend_from_slice(&entry.flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&entry.time.to_le_bytes());
        out.extend_from_slice(&entry.date.to_le_bytes());
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&entry.compressed.to_le_bytes());
        out.extend_from_slice(&entry.uncompressed.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // 额外字段长度
        out.extend_from_slice(&0u16.to_le_bytes()); // 注释长度
        out.extend_from_slice(&0u16.to_le_bytes()); // 起始磁盘号
        out.extend_from_slice(&0u16.to_le_bytes()); // 内部属性
        out.extend_from_slice(&0u32.to_le_bytes()); // 外部属性
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&entry.name);
    }
    let directory_size = out.len() as u32 - directory_offset;
    let count = entries.len() as u16;
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes()); // 磁盘号
    out.extend_from_slice(&0u16.to_le_bytes()); // 中央目录起始磁盘号
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&directory_size.to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // 注释长度
    Ok(out)
}

/// CRC32（IEEE 802.3，zip 用的那个多项式）。
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
