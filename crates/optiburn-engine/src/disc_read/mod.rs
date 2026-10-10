//! 原生读盘：MMC 读块加 ISO 9660 解析，读出盘上末区段的卷标、目录树与文件内容
//! （ADR-0018）。不依赖外部程序，Windows 上读侧只有这条路可走，Linux 维持
//! xorriso，分派规则在 [`crate::readback`]。
//!
//! 末区段的定位序列：READ TOC Format 1 的末区段起始地址（可追加盘上 READ DISC
//! INFORMATION 的字节 5 指向的是尚未写入的开放区段，不能用）。
//!
//! 两种区段地址约定都要认：自家原生引擎把独立镜像原样写进区段，目录记录里的
//! extent 是区段相对地址，xorriso 增长模式写的区段里 extent 是盘级绝对地址
//! （libisofs 的 ms_block 语义，镜像内所有引用都按区段起点位移过）。两种约定的
//! 描述符区（逻辑块 16 起）都物理落在区段起点加偏移，差异只在数据 extent 的解释。
//! 读侧按“根目录首记录是否自引用”探测属于哪种，见 [`probe_address_mode`]。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use hadris_iso::file::EntryType;
use hadris_iso::joliet::JolietLevel;
use hadris_iso::sync::IsoImage;
use hadris_iso::sync::directory::DirectoryRef;
use hadris_iso::sync::read::DirEntry;
use hadris_iso::sync::volume::VolumeDescriptor;
use optiburn_mmc::{DiscStatus, MmcDevice, MmcError, SECTOR_BYTES, wait_until_ready};

use crate::readback::{DiscEntry, ReadBackend, safe_relative_path};
use crate::{BurnError, CancelToken};

/// 一次填补内部缓冲的块数，与 MMC 读侧的单命令上限对齐（64 KiB）。
const CHUNK_BLOCKS: u64 = optiburn_mmc::MAX_READ_BLOCKS as u64;
/// 描述符区扫描的上限：正常镜像 16 起三四扇区内就有终止符，超过这个数按坏盘处理。
const MAX_DESCRIPTOR_SECTORS: u32 = 16;

/// 区段在盘上的地址空间约定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressMode {
    /// 区段相对：源内块号一律加区段起点。自家原生引擎写的区段。
    SessionRelative,
    /// 盘级绝对：源内块号原样作盘上 LBA（xorriso 增长模式的区段）。描述符区例外，
    /// 仍落在区段起点加偏移，长度见 `desc_len`。
    DiscAbsolute { desc_len: u32 },
}

/// 末区段的 ISO 视图字节源：把源内块号按 [`AddressMode`] 映射到盘上 LBA，
/// 实现 std 的 Read 与 Seek 喂给 hadris 的 [`IsoImage`]。
struct SessionSource {
    blocks: Box<dyn BlockSource>,
    base: u32,
    mode: AddressMode,
    /// 源的字节长度（供 `Seek::End`），来自 PVD 的卷空间大小。
    len: u64,
    cancel: CancelToken,
    pos: u64,
    chunk: Vec<u8>,
    chunk_start: u64,
}

/// 按盘上 LBA 读整块的底层源。设备与镜像文件各一个实现，测试另有内存实现。
trait BlockSource {
    fn read_blocks_at(&mut self, lba: u32, out: &mut [u8]) -> Result<(), BurnError>;
}

/// 探测阶段的按块读取：借用块源发的闭包，扫描与地址判定都用它。
type BlockRead<'a> = dyn FnMut(u32, &mut [u8]) -> Result<(), BurnError> + 'a;

/// 光驱：READ(10) 读块（超长自动按 32 块拆，见 mmc 层）。
struct DiscBlocks(MmcDevice);

impl BlockSource for DiscBlocks {
    fn read_blocks_at(&mut self, lba: u32, out: &mut [u8]) -> Result<(), BurnError> {
        self.0.read_blocks(lba, out)?;
        Ok(())
    }
}

/// 镜像文件：ISO 文件本身就是区段相对约定、起点 0 的区段。
struct FileBlocks(File);

impl BlockSource for FileBlocks {
    fn read_blocks_at(&mut self, lba: u32, out: &mut [u8]) -> Result<(), BurnError> {
        self.0
            .seek(SeekFrom::Start(u64::from(lba) * SECTOR_BYTES as u64))?;
        self.0.read_exact(out)?;
        Ok(())
    }
}

impl SessionSource {
    fn new(
        blocks: Box<dyn BlockSource>,
        base: u32,
        mode: AddressMode,
        len: u64,
        cancel: CancelToken,
    ) -> Self {
        Self {
            blocks,
            base,
            mode,
            len,
            cancel,
            pos: 0,
            chunk: Vec::new(),
            chunk_start: 0,
        }
    }

    /// 源内块号到盘上 LBA 的映射。
    fn disc_lba(&self, block: u32) -> u32 {
        match self.mode {
            AddressMode::SessionRelative => self.base.wrapping_add(block),
            AddressMode::DiscAbsolute { desc_len } => {
                if u64::from(block) < 16 + u64::from(desc_len) {
                    self.base.wrapping_add(block)
                } else {
                    block
                }
            }
        }
    }

    /// 保证内部缓冲覆盖 `block`，尽量一次读满 64 KiB，顺序读时每块只发一条命令。
    /// 绝对约定的映射在描述符区边界不连续（区前加起点、区后原样），批量读取不能
    /// 跨过这条边界，块数在那里截断。
    fn fill_chunk(&mut self, block: u64) -> io::Result<()> {
        let in_chunk = !self.chunk.is_empty()
            && block >= self.chunk_start
            && block < self.chunk_start + (self.chunk.len() / SECTOR_BYTES) as u64;
        if in_chunk {
            return Ok(());
        }
        let last_block = self.len.div_ceil(SECTOR_BYTES as u64);
        let mut blocks = CHUNK_BLOCKS.min(last_block - block);
        if let AddressMode::DiscAbsolute { desc_len } = self.mode {
            let descriptor_end = 16 + u64::from(desc_len);
            if block < descriptor_end {
                blocks = blocks.min(descriptor_end - block);
            }
        }
        let blocks = blocks.max(1) as usize;
        self.chunk.resize(blocks * SECTOR_BYTES, 0);
        let lba = self.disc_lba(block as u32);
        self.blocks
            .read_blocks_at(lba, &mut self.chunk)
            .map_err(io::Error::other)?;
        self.chunk_start = block;
        Ok(())
    }
}

impl Read for SessionSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        // 取消不能走 `ErrorKind::Interrupted`：hadris 的 `read_exact` 会无限重试
        // Interrupted（其文档写明 retrying interrupted operations）。这里给一个
        // 普通错误停住读取，外层按令牌状态归因成取消。
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

impl Seek for SessionSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(delta) => self.pos as i64 + delta,
            SeekFrom::End(delta) => self.len as i64 + delta,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session source seeked before zero",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

/// 读侧的来源：Windows 盘符形态（单个字母加冒号）是光驱，其余按镜像文件。
/// 设备串的形态出自 `optiburn_transport::open` 与 `list_optical_devices`。
enum SourceKind {
    Device(String),
    Image(PathBuf),
}

fn classify_source(source: &Path) -> SourceKind {
    let text = source.to_str().unwrap_or_default();
    let bytes = text.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        SourceKind::Device(text.to_string())
    } else {
        SourceKind::Image(source.to_path_buf())
    }
}

/// 原生读侧后端：接在 [`ReadBackend`] 接缝上（ADR-0018）。
pub(crate) struct NativeRead;

impl ReadBackend for NativeRead {
    fn read_volume_id(&self, source: &str) -> Result<String, BurnError> {
        let cancel = CancelToken::default();
        let iso = open_last_session(Path::new(source), &cancel)?;
        volume_id(&iso, &cancel)
    }

    fn last_session_is_iso(&self, source: &str) -> Result<bool, BurnError> {
        match open_last_session(Path::new(source), &CancelToken::default()) {
            Ok(_) => Ok(true),
            // 盘上没有 ISO 9660（空白盘、UDF 盘、音频轨）按“不是”回答，与 xorriso
            // 路径识别兜底空镜像的口径一致，结构损坏等其它错误原样上抛。
            Err(BurnError::NoIsoSession) => Ok(false),
            Err(other) => Err(other),
        }
    }

    fn list_tree(&self, source: &str) -> Result<Vec<DiscEntry>, BurnError> {
        let cancel = CancelToken::default();
        let iso = open_last_session(Path::new(source), &cancel)?;
        let root = iso.root_dir();
        let joliet = is_joliet_root(&root.entry_type());
        let mut entries = Vec::new();
        walk_list(&iso, root.dir_ref(), "", joliet, &mut entries, &cancel)?;
        Ok(entries)
    }

    fn extract_tree(
        &self,
        source: &Path,
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        let iso = open_last_session(source, cancel)?;
        let root = iso.root_dir();
        let joliet = is_joliet_root(&root.entry_type());
        extract_dir(&iso, root.dir_ref(), joliet, dest, cancel)
    }

    fn extract_paths(
        &self,
        source: &str,
        paths: &[String],
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        let iso = open_last_session(Path::new(source), cancel)?;
        let joliet = is_joliet_root(&iso.root_dir().entry_type());
        for path in paths {
            // 先过安全过滤再碰盘：不安全路径没有必要读盘，也避免中途才发现要拒绝。
            let target = dest.join(safe_relative_path(path)?);
            // find_path 的逐段匹配覆盖三种命名空间（RRIP 精确、Joliet 解码后比较、
            // 平面名忽略大小写）。
            let Some(entry) = iso.find_path(path).map_err(|e| hadris_error(e, cancel))? else {
                return Err(BurnError::ReadFailed(format!(
                    "path not found on the disc: {path}"
                )));
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if entry.is_directory() {
                let dir = entry
                    .as_dir_ref(&iso)
                    .map_err(|e| hadris_error(e, cancel))?;
                extract_dir(&iso, dir, joliet, &target, cancel)?;
            } else {
                let data = iso.read_file(&entry).map_err(|e| hadris_error(e, cancel))?;
                std::fs::write(&target, &data)?;
            }
        }
        Ok(())
    }
}

/// 打开末区段的 ISO 视图。盘上（或文件里）没有可读的 ISO 9660 时报
/// [`BurnError::NoIsoSession`]。
fn open_last_session(
    source: &Path,
    cancel: &CancelToken,
) -> Result<IsoImage<SessionSource>, BurnError> {
    match classify_source(source) {
        SourceKind::Image(path) => {
            let file = File::open(&path)?;
            let len = file.metadata()?.len();
            if len < SECTOR_BYTES as u64 * 17 {
                return Err(BurnError::NoIsoSession);
            }
            open_session_view(FileBlocks(file), 0, len, cancel)
        }
        SourceKind::Device(device) => {
            let transport = optiburn_transport::open(&device)
                .map_err(|e| BurnError::Mmc(MmcError::Transport(e)))?;
            let mut mmc = MmcDevice::new(transport);
            wait_until_ready(&mut mmc)?;
            // 空白盘先看盘片状态归到 NoIsoSession：没有已完结区段时 READ TOC
            // Format 1 按驱动器不同可能回短数据也可能报命令失败，先查状态才
            // 不会把空盘当设备错误（ADR-0018 决策 6，GUI 空盘浏览回空清单）。
            let info = mmc.read_disc_information()?;
            if info.status == DiscStatus::Empty {
                return Err(BurnError::NoIsoSession);
            }
            let Some(session) = mmc.read_toc_session_info()? else {
                return Err(BurnError::NoIsoSession);
            };
            open_session_view(
                DiscBlocks(mmc),
                session.last_session_start,
                u64::MAX,
                cancel,
            )
        }
    }
}

/// 在起点 `base` 的区段上探测地址空间约定并打开 ISO 视图。`len_hint` 只在
/// 调用方已知源长度时给出（镜像文件），设备路径的长度从 PVD 的卷空间大小取。
fn open_session_view<B: BlockSource + 'static>(
    mut blocks: B,
    base: u32,
    len_hint: u64,
    cancel: &CancelToken,
) -> Result<IsoImage<SessionSource>, BurnError> {
    let layout = {
        let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
        scan_descriptors(&mut read, base)?.ok_or(BurnError::NoIsoSession)?
    };
    let relative = {
        let mut read = |lba: u32, out: &mut [u8]| blocks.read_blocks_at(lba, out);
        probe_address_mode(&mut read, base, layout.root_extent)?
    };
    let mode = if relative {
        AddressMode::SessionRelative
    } else {
        AddressMode::DiscAbsolute {
            desc_len: layout.desc_len,
        }
    };
    let len = match len_hint {
        hint if hint < u64::MAX => hint,
        // vss 在两种约定下都是区段相对大小（实测 xorriso 增长区段：起点 150093 的
        // 区段 vss=107575、根 extent 150112 是绝对地址），长度一律按 base+vss 算。
        // 它只用作 Seek::End 与读越界判断，偏一点只影响坏记录的判定。
        _ => u64::from(base + layout.volume_blocks) * SECTOR_BYTES as u64,
    };
    let source = SessionSource::new(Box::new(blocks), base, mode, len, cancel.clone());
    IsoImage::open(source).map_err(|e| {
        // 令牌已置位时按取消归因（理由见 hadris_error）。
        if cancel.is_cancelled() {
            BurnError::Cancelled
        } else {
            BurnError::ReadFailed(format!(
                "failed to parse the last session's ISO 9660 structure: {e}"
            ))
        }
    })
}

/// 描述符区扫描结果：区段布局的关键字段。
struct SessionLayout {
    /// 描述符区的长度（含终止符，从逻辑块 16 数），绝对约定的映射要用。
    desc_len: u32,
    /// PVD 根目录记录里的 extent（偏移 156 的记录，extent 在 158，小端）。
    root_extent: u32,
    /// PVD 的卷空间大小（偏移 80，小端）。
    volume_blocks: u32,
}

/// 从区段起点加 16 扫描述符区。没有 PVD（CD001 且类型 1）返回 `None`：
/// UDF 盘那里是 BEA01/NSR02 序列，空白盘根本没有区段，都归到这里。
fn scan_descriptors(read: &mut BlockRead, base: u32) -> Result<Option<SessionLayout>, BurnError> {
    let mut pvd: Option<[u8; SECTOR_BYTES]> = None;
    let mut sectors = 0u32;
    for index in 0..MAX_DESCRIPTOR_SECTORS {
        let mut sector = [0u8; SECTOR_BYTES];
        read(base + 16 + index, &mut sector)?;
        if sector[1..6] != *b"CD001" {
            break;
        }
        sectors = index + 1;
        if sector[0] == 1 && pvd.is_none() {
            pvd = Some(sector);
        }
        if sector[0] == 255 {
            break;
        }
    }
    let Some(pvd) = pvd else {
        return Ok(None);
    };
    Ok(Some(SessionLayout {
        desc_len: sectors,
        root_extent: u32::from_le_bytes([pvd[158], pvd[159], pvd[160], pvd[161]]),
        volume_blocks: u32::from_le_bytes([pvd[80], pvd[81], pvd[82], pvd[83]]),
    }))
}

/// 探测区段的地址空间约定：读根目录 extent 所在块，看它的首条目录记录（“.”）
/// 是否自引用。相对约定的记录里写的是相对块号（等于 E），绝对约定里写的是盘上
/// LBA（同样等于 E，因为两种约定下这个字段都指向根目录自身所在的源内块）。
/// 判定用“读到的块号 == 记录声称的块号”，按相对、绝对的顺序尝试。相对候选的
/// 读取失败（绝对约定的盘上 base+E 可能越过可读范围，驱动器报错）不算结论，
/// 换绝对候选再判。
fn probe_address_mode(
    read: &mut BlockRead,
    base: u32,
    root_extent: u32,
) -> Result<bool, BurnError> {
    let mut sector = [0u8; SECTOR_BYTES];
    let relative_hit = read(base + root_extent, &mut sector)
        .is_ok_and(|()| root_record_is_self_referential(&sector, root_extent));
    if relative_hit {
        return Ok(true);
    }
    read(root_extent, &mut sector)?;
    if root_record_is_self_referential(&sector, root_extent) {
        return Ok(false);
    }
    Err(BurnError::ReadFailed(
        "the last session's ISO layout matches neither addressing convention".to_string(),
    ))
}

/// 根目录首记录（“.”）是否自引用：记录长度不小于 34、目录标志位置位、extent
/// 字段（偏移 2，小端）等于所在块的源内块号。首区段（base 为 0）两种约定重合，
/// 按相对处理即可。
fn root_record_is_self_referential(sector: &[u8], extent: u32) -> bool {
    sector.len() == SECTOR_BYTES
        && sector[0] >= 34
        && sector[25] & 0x02 != 0
        && u32::from_le_bytes([sector[2], sector[3], sector[4], sector[5]]) == extent
}

/// 读卷标：PVD 的卷标识（偏移 40，d 字符），为空时回退 Joliet SVD 的 UTF-16BE 值。
/// 空白或全空格的 PVD 值没有信息量，常见于卷标留空的镜像。
fn volume_id(iso: &IsoImage<SessionSource>, cancel: &CancelToken) -> Result<String, BurnError> {
    let mut joliet_id = None;
    for descriptor in iso.read_volume_descriptors() {
        let descriptor = descriptor.map_err(|e| hadris_error(e, cancel))?;
        match descriptor {
            VolumeDescriptor::Primary(pvd) => {
                let text = decode_d_string(pvd.volume_identifier.as_bytes());
                if !text.is_empty() {
                    return Ok(text);
                }
            }
            VolumeDescriptor::Supplementary(svd)
                if joliet_id.is_none()
                    && JolietLevel::from_escape_sequence(&svd.escape_sequences).is_some() =>
            {
                joliet_id = Some(decode_utf16_be(svd.volume_identifier.as_bytes()));
            }
            _ => {}
        }
    }
    joliet_id
        .filter(|text| !text.is_empty())
        .ok_or_else(|| BurnError::ReadFailed("no readable volume id".to_string()))
}

/// PVD 的 d 字符串解码：去尾部空格与 NUL，按 UTF-8 有损转换（个别镜像不守字符集
/// 约定，宁可显示替换符也不要失败）。
fn decode_d_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .rposition(|b| *b != b' ' && *b != 0)
        .map_or(0, |index| index + 1);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Joliet 的 UTF-16BE 解码（卷标识用，文件名走 hadris 的解码）。
fn decode_utf16_be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units).trim_end().to_string()
}

/// 根是否 Joliet 命名空间（hadris 的 best_choice 在有 RRIP 的主命名空间时优先
/// 选它，RRIP 名与 Joliet 名都能保留大小写与中文，两者取一即可）。
fn is_joliet_root(entry_type: &EntryType) -> bool {
    matches!(entry_type, EntryType::Joliet { .. })
}

/// 条目名解码：Joliet 命名空间按 UTF-16BE 解（hadris 的 display_name 不解 Joliet，
/// 中文会变替换符），其余用 RRIP 优先的显示名。最后去掉尾部 NUL 填充与 `;1`
/// 版本号后缀。
fn entry_name(entry: &DirEntry, joliet: bool) -> String {
    let mut name = if joliet {
        entry.record.joliet_name()
    } else {
        entry.display_name().into_owned()
    };
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

/// “.”与“..”记录：名字是单个字节 0 或 1。必须限定长度，Joliet 名是 UTF-16BE，
/// ASCII 名的首字节也是 0（“a”编码为 `00 61`），不限长度会把这类条目整个吞掉。
fn is_self_reference(entry: &DirEntry) -> bool {
    let name = entry.name();
    name.len() == 1 && matches!(name[0], 0 | 1)
}

/// 深度优先列目录树，顺序即目录记录顺序（xorriso 的 -find 也是树序）。
fn walk_list(
    iso: &IsoImage<SessionSource>,
    dir: DirectoryRef,
    prefix: &str,
    joliet: bool,
    out: &mut Vec<DiscEntry>,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    for entry in iso.open_dir(dir).entries() {
        let entry = entry.map_err(|e| hadris_error(e, cancel))?;
        if is_self_reference(&entry) {
            continue;
        }
        let path = format!("{}/{}", prefix, entry_name(&entry, joliet));
        let is_dir = entry.is_directory();
        out.push(DiscEntry {
            size: if is_dir { 0 } else { entry.total_size() },
            path: path.clone(),
            is_dir,
        });
        if is_dir {
            let sub = entry.as_dir_ref(iso).map_err(|e| hadris_error(e, cancel))?;
            walk_list(iso, sub, &path, joliet, out, cancel)?;
        }
    }
    Ok(())
}

/// 把一个目录树抽到本地。名字按盘上记录落盘，写盘前逐段过安全过滤（盘内容不是
/// 可信输入，与 xorriso 路径共用 [`safe_relative_path`] 的规则）。
fn extract_dir(
    iso: &IsoImage<SessionSource>,
    dir: DirectoryRef,
    joliet: bool,
    dest: &Path,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    std::fs::create_dir_all(dest)?;
    for entry in iso.open_dir(dir).entries() {
        if cancel.is_cancelled() {
            return Err(BurnError::Cancelled);
        }
        let entry = entry.map_err(|e| hadris_error(e, cancel))?;
        if is_self_reference(&entry) {
            continue;
        }
        let name = entry_name(&entry, joliet);
        let target = dest.join(safe_relative_path(&format!("/{name}"))?);
        if entry.is_directory() {
            let sub = entry.as_dir_ref(iso).map_err(|e| hadris_error(e, cancel))?;
            extract_dir(iso, sub, joliet, &target, cancel)?;
        } else {
            let data = iso.read_file(&entry).map_err(|e| hadris_error(e, cancel))?;
            std::fs::write(&target, &data)?;
        }
    }
    Ok(())
}

/// hadris 的读错误到读侧错误的归一。令牌已置位时按取消归因：读中途的取消在
/// 传输层表现为普通 io 错误（见 [`SessionSource::read`] 的注释），令牌是唯一
/// 可靠的判据。读出错与用户点中止同时发生时，报取消比报故障更贴近用户视角。
fn hadris_error(error: hadris_iso::sync::Error, cancel: &CancelToken) -> BurnError {
    if cancel.is_cancelled() {
        BurnError::Cancelled
    } else {
        BurnError::ReadFailed(format!("disc read failed: {error}"))
    }
}

#[cfg(test)]
mod tests;
