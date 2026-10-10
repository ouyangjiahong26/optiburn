//! UDF 读侧：把末区段包装成 hadris-udf 能直接读的字节源（ADR-0021）。
//!
//! 为什么需要这一层：UDF 的发现位置（VRS 从区段起点加 16 起，AVDP 探测位在加
//! 256 与加 512）按区段内的固定偏移定位，而描述符内容里的地址有两种约定——
//! 自家原生引擎把独立镜像原样写进区段，地址是区段相对；标准写入器（Windows
//! 刻的多区段盘）写盘级绝对地址（内核 fs/udf/super.c 的 udf_read_ptagged 语义：
//! 固定位置在区段内偏移处发现，内容地址当物理块用）。hadris-udf 只会按“块 256”
//! 找锚点，且只在 tag_location 与它请求的块号一致时才通过校验，所以由这一层把
//! 两种约定归一化成它期望的视图。
//!
//! 归一化只动 hadris 的校验前提，不动描述符里的内容地址：
//! - 请求块到物理块的映射按探测出的约定换算（见 [`UdfMapping`]）；
//! - 绝对约定下，固定发现位置读到的 AVDP 在盘上带的是物理块号，改写成本层
//!   虚拟的“区段相对块号”并重算 tag 校验和。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use hadris_udf::UdfVolume;
use hadris_udf::dir::{UdfDir, UdfDirEntry};
use optiburn_mmc::{DiscStatus, MmcDevice, MmcError, SECTOR_BYTES, wait_until_ready};

use super::{BlockSource, DiscBlocks, FileBlocks, SourceKind, classify_source};
use crate::readback::{DiscEntry, safe_relative_path};
use crate::{BurnError, CancelToken};

/// 一次填补内部缓冲的块数，与 MMC 读侧的单命令上限对齐（64 KiB）。
const CHUNK_BLOCKS: u64 = optiburn_mmc::MAX_READ_BLOCKS as u64;
/// VRS 扫描的块数，与 hadris-udf 的 `parse_vrs` 一致（区段起点加 16 起 16 块）。
const VRS_SECTORS: u32 = 16;
/// AVDP 的 tag 标识（ECMA-167 3/10.2）。
const AVDP_TAG: u16 = 2;
/// AVDP 探测的两个固定位置（区段内偏移）。
const ANCHOR_OFFSETS: [u32; 2] = [256, 512];
/// 目录递归的深度上限。UDF 允许的层数很大，真盘不会接近这个数；上限只为拦住
/// 损坏镜像里指回祖先的目录项造成的无限递归。
const MAX_DIRECTORY_DEPTH: usize = 64;
/// 设备路径的源长度：要探测加打开之后才知道卷大小，打开时只能用哨兵值。取
/// `i64::MAX` 而不是 `u64::MAX`，因为 `Seek::End` 要把长度当 i64 用（hadris
/// 读文件前会 `seek(End(0))` 做越界判断，`u64::MAX` 转 i64 会变负数）。读到源
/// 之外由传输层报错，不影响正确性。
const UNKNOWN_LENGTH: u64 = u64::MAX / 2;

/// 描述符里的地址约定，探测自 AVDP 的 tag_location。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UdfMapping {
    /// 源内块号就是物理块号：镜像文件，或首区段。
    Single,
    /// 内容地址是区段相对：发现位置与内容块都加区段起点。
    SessionRelative,
    /// 内容地址是盘级绝对：发现位置加区段起点，内容块原样。
    DiscAbsolute,
}

/// 末区段的 UDF 视图字节源：把 hadris-udf 请求的块号按 [`UdfMapping`] 映射到
/// 盘上物理块，实现 std 的 Read 与 Seek 喂给 [`UdfVolume`]。
pub(crate) struct UdfSource {
    blocks: Box<dyn BlockSource>,
    base: u32,
    mapping: UdfMapping,
    /// 源的字节长度（供 `Seek::End`），设备路径给 `u64::MAX`。
    len: u64,
    cancel: CancelToken,
    pos: u64,
    chunk: Vec<u8>,
    chunk_start: u64,
}

impl UdfSource {
    fn new(
        blocks: Box<dyn BlockSource>,
        base: u32,
        mapping: UdfMapping,
        len: u64,
        cancel: CancelToken,
    ) -> Self {
        Self {
            blocks,
            base,
            mapping,
            len,
            cancel,
            pos: 0,
            chunk: Vec::new(),
            chunk_start: 0,
        }
    }

    /// 请求块号到盘上物理块的映射。绝对约定下内容地址本来就是物理块号，只有
    /// 固定发现位置（区段内偏移 16 到 31、256、512）在区段起点加偏移；真实区段
    /// 起点必然大于这些偏移，两种情形不会撞车（见模块头）。
    fn physical_lba(&self, block: u32) -> u32 {
        match self.mapping {
            UdfMapping::Single => block,
            UdfMapping::SessionRelative => self.base.wrapping_add(block),
            UdfMapping::DiscAbsolute if block >= self.base => block,
            UdfMapping::DiscAbsolute => self.base.wrapping_add(block),
        }
    }

    /// 块号是不是 UDF 的固定发现位置（区段内偏移固定，绝对约定下要归一化）。
    fn is_discovery_block(block: u64) -> bool {
        (16..16 + u64::from(VRS_SECTORS)).contains(&block)
            || ANCHOR_OFFSETS.contains(&(block as u32))
    }

    /// 保证内部缓冲覆盖 `block`，尽量一次读满 64 KiB。物理映射在固定发现位置与
    /// 内容区之间不连续，块数在那里截断，批量读取才不会跨过不连续的边界。
    fn fill_chunk(&mut self, block: u64) -> io::Result<()> {
        let in_chunk = !self.chunk.is_empty()
            && block >= self.chunk_start
            && block < self.chunk_start + (self.chunk.len() / SECTOR_BYTES) as u64;
        if in_chunk {
            return Ok(());
        }
        let last_block = self.len.div_ceil(SECTOR_BYTES as u64);
        let blocks = CHUNK_BLOCKS.min(last_block.saturating_sub(block)).max(1);
        let start_lba = u64::from(self.physical_lba(block as u32));
        let mut contiguous = 1u64;
        while contiguous < blocks
            && u64::from(self.physical_lba((block + contiguous) as u32)) == start_lba + contiguous
        {
            contiguous += 1;
        }
        let blocks = contiguous as usize;
        self.chunk.resize(blocks * SECTOR_BYTES, 0);
        self.blocks
            .read_blocks_at(start_lba as u32, &mut self.chunk)
            .map_err(io::Error::other)?;
        self.chunk_start = block;
        self.normalize_anchors(blocks);
        Ok(())
    }

    /// 绝对约定下，固定发现位置读到的 AVDP 在盘上带的是物理块号，而 hadris-udf
    /// 拿“请求块号”（它只请求 256）去校验 tag。把缓冲里这些块的 tag_location
    /// 改写成请求块号并重算 tag 校验和（字节 4 是字节 0 到 3 与 5 到 15 之和，
    /// 与 hadris-udf 的 `DescriptorTag` 同口径）。其余块原样——描述符内容里的
    /// 地址本来就是绝对地址，不能碰。
    fn normalize_anchors(&mut self, blocks: usize) {
        if self.mapping != UdfMapping::DiscAbsolute {
            return;
        }
        for index in 0..blocks as u64 {
            let block = self.chunk_start + index;
            if !Self::is_discovery_block(block) {
                continue;
            }
            let start = index as usize * SECTOR_BYTES;
            let tag = &mut self.chunk[start..start + 16];
            if u16::from_le_bytes([tag[0], tag[1]]) != AVDP_TAG {
                continue;
            }
            tag[12..16].copy_from_slice(&(block as u32).to_le_bytes());
            tag[4] = tag
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != 4)
                .fold(0u8, |sum, (_, byte)| sum.wrapping_add(*byte));
        }
    }
}

impl Read for UdfSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        // 取消不能走 `ErrorKind::Interrupted`：hadris 的 `read_exact` 会无限重试
        // Interrupted。这里给普通错误停住读取，外层按令牌状态归因成取消。
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("disc read cancelled"));
        }
        let block = self.pos / SECTOR_BYTES as u64;
        let offset = (self.pos % SECTOR_BYTES as u64) as usize;
        self.fill_chunk(block)?;
        let start = (block - self.chunk_start) as usize * SECTOR_BYTES + offset;
        let available = self.chunk.len() - start;
        let wanted = buf.len().min(available).min((self.len - self.pos) as usize);
        buf[..wanted].copy_from_slice(&self.chunk[start..start + wanted]);
        self.pos += wanted as u64;
        Ok(wanted)
    }
}

impl Seek for UdfSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(delta) => self.pos as i64 + delta,
            SeekFrom::End(delta) => self.len as i64 + delta,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDF source seeked before zero",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

/// 打开末区段的 UDF 视图。源上没有 UDF（空白盘、音频盘、纯 ISO 盘）时报
/// [`BurnError::NoIsoSession`]；是 UDF 但结构超出 hadris-udf 的能力（VAT、元数据
/// 分区、锚点位置不认识等）时报 [`BurnError::UnsupportedUdf`]。
pub(crate) fn open_udf_session(
    source: &Path,
    cancel: &CancelToken,
) -> Result<UdfVolume<UdfSource>, BurnError> {
    match classify_source(source) {
        SourceKind::Image(path) => {
            let file = File::open(&path)?;
            let len = file.metadata()?.len();
            // 连 VRS 都放不下的短文件直接归到“没有 UDF”，与 ISO 侧同一道门。
            if len < u64::from(16 + VRS_SECTORS) * SECTOR_BYTES as u64 {
                return Err(BurnError::NoIsoSession);
            }
            open_udf_view(Box::new(FileBlocks(file)), 0, len, cancel)
        }
        SourceKind::Device(device) => {
            let transport = optiburn_transport::open(&device)
                .map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?;
            let mut mmc = MmcDevice::new(transport);
            wait_until_ready(&mut mmc)?;
            // 空白盘先按盘片状态归到 NoIsoSession，与 ISO 侧同一道门：没有区段
            // 就没有 VRS，READ TOC 按驱动器不同也可能报命令失败。
            if mmc.read_disc_information()?.status == DiscStatus::Empty {
                return Err(BurnError::NoIsoSession);
            }
            // 与 ISO 侧相同：末区段起点取自 READ TOC Format 1，取不到按首区段。
            let base = mmc
                .read_toc_session_info()?
                .map_or(0, |session| session.last_session_start);
            open_udf_view(Box::new(DiscBlocks(mmc)), base, UNKNOWN_LENGTH, cancel)
        }
    }
}

/// 在起点 `base` 的区段上探测并打开 UDF 视图。`len` 是源的字节长度（设备用
/// [`UNKNOWN_LENGTH`]）；测试用内存块源直接调它，所以是 crate 可见。
pub(crate) fn open_udf_view(
    mut blocks: Box<dyn BlockSource>,
    base: u32,
    len: u64,
    cancel: &CancelToken,
) -> Result<UdfVolume<UdfSource>, BurnError> {
    if !has_vrs(&mut *blocks, base, cancel)? {
        return Err(BurnError::NoIsoSession);
    }
    let mapping = probe_mapping(&mut *blocks, base)?;
    let source = UdfSource::new(blocks, base, mapping, len, cancel.clone());
    UdfVolume::open(source).map_err(|error| {
        if cancel.is_cancelled() {
            return BurnError::Cancelled;
        }
        // VRS 已经认出来了，打不开就是结构超出 hadris-udf 的能力（VAT、元数据
        // 分区、扩展分配描述符等），不当“没有 UDF”。
        BurnError::UnsupportedUdf(error.to_string())
    })
}

/// VRS 检查：区段起点加 16 起 16 块内要先出现 `BEA01` 再出现 `NSR02`/`NSR03`。
/// 空白盘、音频盘与纯 ISO 盘在这里落回 [`BurnError::NoIsoSession`]。判据与
/// hadris-udf 的 `parse_vrs` 相同（结构类型 0、版本 1），步调也一致。
fn has_vrs(
    blocks: &mut dyn BlockSource,
    base: u32,
    cancel: &CancelToken,
) -> Result<bool, BurnError> {
    let mut bea01 = false;
    for index in 0..VRS_SECTORS {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let mut sector = [0u8; SECTOR_BYTES];
        blocks.read_blocks_at(base.wrapping_add(16 + index), &mut sector)?;
        if sector[0] != 0 || sector[6] != 1 {
            continue;
        }
        match &sector[1..6] {
            b"BEA01" => bea01 = true,
            b"NSR02" | b"NSR03" if bea01 => return Ok(true),
            _ => {}
        }
    }
    Ok(false)
}

/// 探测地址约定：读固定位置 `base + 256`（读不到或不是 AVDP 时再试 `base + 512`）
/// 的整块，看它的 tag_location。等于区段内偏移说明内容地址是区段相对；等于物理
/// 块号说明是盘级绝对（标准写入器的多区段盘）。起点为 0 时两种约定重合。
fn probe_mapping(blocks: &mut dyn BlockSource, base: u32) -> Result<UdfMapping, BurnError> {
    for offset in ANCHOR_OFFSETS {
        let mut sector = [0u8; SECTOR_BYTES];
        if blocks
            .read_blocks_at(base.wrapping_add(offset), &mut sector)
            .is_err()
        {
            continue;
        }
        if u16::from_le_bytes([sector[0], sector[1]]) != AVDP_TAG {
            continue;
        }
        if base == 0 {
            return Ok(UdfMapping::Single);
        }
        let location = u32::from_le_bytes([sector[12], sector[13], sector[14], sector[15]]);
        if location == offset {
            return Ok(UdfMapping::SessionRelative);
        }
        if location == base.wrapping_add(offset) {
            return Ok(UdfMapping::DiscAbsolute);
        }
        return Err(BurnError::UnsupportedUdf(
            "unrecognized UDF anchor addressing".to_string(),
        ));
    }
    // VRS 已认出来却没有固定位置的锚点：这份 UDF 的锚点在别处（VAT、包写盘等）。
    Err(BurnError::UnsupportedUdf(
        "no UDF anchor volume descriptor pointer at the session start".to_string(),
    ))
}

/// 读 UDF 卷标：PVD 的 dstring，去尾部 NUL 与空格。空值与 ISO 侧同口径地报
/// “读不到卷标”。
pub(crate) fn volume_id(volume: &UdfVolume<UdfSource>) -> Result<String, BurnError> {
    let id = volume.info().volume_id.trim_end_matches('\0').trim_end();
    if id.is_empty() {
        return Err(BurnError::ReadFailed("no readable volume id".to_string()));
    }
    Ok(id.to_string())
}

/// 深度优先列 UDF 目录树，顺序即目录记录顺序。
pub(crate) fn list_tree(
    volume: &UdfVolume<UdfSource>,
    cancel: &CancelToken,
) -> Result<Vec<DiscEntry>, BurnError> {
    let root = volume.root_dir().map_err(|e| udf_error(e, cancel))?;
    let mut out = Vec::new();
    walk_list(volume, &root, "", 0, &mut out, cancel)?;
    Ok(out)
}

fn walk_list(
    volume: &UdfVolume<UdfSource>,
    dir: &UdfDir,
    prefix: &str,
    depth: usize,
    out: &mut Vec<DiscEntry>,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    check_depth(depth)?;
    for entry in dir.entries() {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let path = format!("{}/{}", prefix, entry.name);
        out.push(DiscEntry {
            path: path.clone(),
            size: if entry.is_directory { 0 } else { entry.size },
            is_dir: entry.is_directory,
        });
        if entry.is_directory {
            let sub = volume
                .read_directory(&entry.icb)
                .map_err(|e| udf_error(e, cancel))?;
            walk_list(volume, &sub, &path, depth + 1, out, cancel)?;
        }
    }
    Ok(())
}

/// 把整棵 UDF 树抽到本地。名字按盘上记录落盘，逐段过安全过滤（盘内容不是可信
/// 输入，与 ISO 侧共用 [`safe_relative_path`] 的规则）。
pub(crate) fn extract_tree(
    volume: &UdfVolume<UdfSource>,
    dest: &Path,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    let root = volume.root_dir().map_err(|e| udf_error(e, cancel))?;
    extract_dir(volume, &root, dest, 0, cancel)
}

fn extract_dir(
    volume: &UdfVolume<UdfSource>,
    dir: &UdfDir,
    dest: &Path,
    depth: usize,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    check_depth(depth)?;
    std::fs::create_dir_all(dest)?;
    for entry in dir.entries() {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let target = dest.join(safe_relative_path(&format!("/{}", entry.name))?);
        if entry.is_directory {
            let sub = volume
                .read_directory(&entry.icb)
                .map_err(|e| udf_error(e, cancel))?;
            extract_dir(volume, &sub, &target, depth + 1, cancel)?;
        } else {
            let data = volume.read_file(entry).map_err(|e| udf_error(e, cancel))?;
            std::fs::write(&target, &data)?;
        }
    }
    Ok(())
}

/// 从盘上按 UDF 路径抽取若干文件或目录到本地目录。逐段按名字精确匹配（大小写
/// 敏感）：UDF 记录里存的就是最终名，与 ISO 侧 `find_path` 的多命名空间回退不同。
pub(crate) fn extract_paths(
    volume: &UdfVolume<UdfSource>,
    paths: &[String],
    dest: &Path,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    for path in paths {
        // 先过安全过滤再碰盘：不安全路径没有必要读盘，也避免中途才发现要拒绝。
        let target = dest.join(safe_relative_path(path)?);
        let Some(entry) = find_entry(volume, path, cancel)? else {
            return Err(BurnError::ReadFailed(format!(
                "path not found on the disc: {path}"
            )));
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if entry.is_directory {
            let dir = volume
                .read_directory(&entry.icb)
                .map_err(|e| udf_error(e, cancel))?;
            extract_dir(volume, &dir, &target, 0, cancel)?;
        } else {
            let data = volume.read_file(&entry).map_err(|e| udf_error(e, cancel))?;
            std::fs::write(&target, &data)?;
        }
    }
    Ok(())
}

/// 按盘上路径逐级查条目。路径段为空（前导斜杠、连续斜杠）忽略；中间段不是目录
/// 或名字不匹配都算找不到。
fn find_entry(
    volume: &UdfVolume<UdfSource>,
    path: &str,
    cancel: &CancelToken,
) -> Result<Option<UdfDirEntry>, BurnError> {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let Some((last, parents)) = segments.split_last() else {
        return Ok(None);
    };
    let mut dir = volume.root_dir().map_err(|e| udf_error(e, cancel))?;
    for segment in parents {
        let Some(entry) = dir.find(segment).cloned() else {
            return Ok(None);
        };
        if !entry.is_directory {
            return Ok(None);
        }
        dir = volume
            .read_directory(&entry.icb)
            .map_err(|e| udf_error(e, cancel))?;
    }
    Ok(dir.find(last).cloned())
}

/// 目录深度门禁：拦住损坏镜像里指回祖先的目录项。
fn check_depth(depth: usize) -> Result<(), BurnError> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Err(BurnError::UnsupportedUdf(
            "directory nesting deeper than 64 levels".to_string(),
        ));
    }
    Ok(())
}

/// hadris-udf 的错误到读侧错误的归一。令牌已置位时按取消归因：读中途的取消在
/// 传输层表现为普通 io 错误（见 [`UdfSource::read`] 的注释），令牌是唯一可靠的
/// 判据。
fn udf_error(error: hadris_udf::Error, cancel: &CancelToken) -> BurnError {
    if cancel.is_cancelled() {
        BurnError::Cancelled
    } else {
        BurnError::ReadFailed(format!("disc read failed: {error}"))
    }
}

#[cfg(test)]
mod tests;
