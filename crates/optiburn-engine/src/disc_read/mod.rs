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
//!
//! 没有 ISO 9660 的末区段再试 UDF（[`udf`] 模块，ADR-0021）：Windows 刻的纯 UDF
//! 盘与 Bridge 盘的后半段都走那条路，两个都没有才是 [`BurnError::NoIsoSession`]。

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use hadris_iso::file::EntryType;
use hadris_iso::joliet::JolietLevel;
use hadris_iso::sync::IsoImage;
use hadris_iso::sync::directory::DirectoryRef;
use hadris_iso::sync::read::DirEntry;
use hadris_iso::sync::volume::VolumeDescriptor;
use hadris_udf::UdfVolume;
use optiburn_mmc::{DiscStatus, MmcDevice, MmcError, SECTOR_BYTES, wait_until_ready};

use self::udf::UdfSource;
use crate::readback::{DiscEntry, ReadBackend, safe_relative_path};
use crate::{BurnError, CancelToken, IsoSessionState, SessionFallback};

mod udf;

/// 一次填补内部缓冲的块数，与 MMC 读侧的单命令上限对齐（64 KiB）。
const CHUNK_BLOCKS: u64 = optiburn_mmc::MAX_READ_BLOCKS as u64;
/// 描述符区扫描的上限：正常镜像 16 起三四扇区内就有终止符，超过这个数按坏盘处理。
const MAX_DESCRIPTOR_SECTORS: u32 = 16;

/// 区段在盘上的地址约定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddressMode {
    /// 区段相对：源内块号一律加区段起点。自家原生引擎写的区段。
    SessionRelative,
    /// 盘级绝对：源内块号原样作盘上 LBA（xorriso 增长模式的区段）。描述符区例外，
    /// 仍落在区段起点加偏移，长度见 `desc_len`。
    DiscAbsolute { desc_len: u32 },
}

/// 区段的源内块号到盘上 LBA 的映射。描述符区（逻辑块 16 起，长度 `desc_len`）
/// 在两种约定下都在区段起点加偏移，绝对约定下批量读取也不能跨过这条边界
/// （映射在那里不连续）。
pub(crate) fn mapped_lba(mode: AddressMode, base: u32, block: u32) -> u32 {
    match mode {
        AddressMode::SessionRelative => base.wrapping_add(block),
        AddressMode::DiscAbsolute { desc_len } => {
            if u64::from(block) < 16 + u64::from(desc_len) {
                base.wrapping_add(block)
            } else {
                block
            }
        }
    }
}

/// 末区段的 ISO 视图字节源：把源内块号按 [`AddressMode`] 映射到盘上 LBA，
/// 实现 std 的 Read 与 Seek 喂给 hadris 的 [`IsoImage`]。
pub(crate) struct SessionSource {
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
pub(crate) trait BlockSource {
    fn read_blocks_at(&mut self, lba: u32, out: &mut [u8]) -> Result<(), BurnError>;
}

/// 探测阶段的按块读取：借用块源发的闭包，扫描与地址判定都用它。增长模式读旧
/// 区段（[`crate::grow`]）也用它，所以是 crate 可见。
pub(crate) type BlockRead<'a> = dyn FnMut(u32, &mut [u8]) -> Result<(), BurnError> + 'a;

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

/// 多个候选视图共享一台设备：逐个候选尝试时不能每个都拿走设备所有权（见
/// [`open_readable_session`]）。单线程使用，`Rc<RefCell>` 就够。
struct SharedBlocks(Rc<RefCell<Box<dyn BlockSource>>>);

impl BlockSource for SharedBlocks {
    fn read_blocks_at(&mut self, lba: u32, out: &mut [u8]) -> Result<(), BurnError> {
        self.0.borrow_mut().read_blocks_at(lba, out)
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
        mapped_lba(self.mode, self.base, block)
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

/// 末区段的会话视图：ISO 9660 或 UDF。读侧四个入口共用（ADR-0018、ADR-0021）。
pub(crate) enum DiscSession {
    Iso(IsoImage<SessionSource>),
    Udf(UdfVolume<UdfSource>),
}

/// 打开末区段的会话视图：先按 ISO 9660 开，只有“没有 ISO 9660”时才再试 UDF。
/// Bridge 盘因此走 ISO 分支，与追加门禁的语义保持一致（那些盘续写 ISO 区段会
/// 遮住原有内容，见 [`crate::readback::last_session_is_iso`]）。
///
/// 末区段损坏时按 [`open_readable_session`] 的候选回退；回退信息只在这一层丢掉，
/// 需要告知用户的调用方走 [`ReadBackend::session_fallback`]。
pub(crate) fn open_session(source: &Path, cancel: &CancelToken) -> Result<DiscSession, BurnError> {
    match open_readable_session(source, cancel) {
        Ok(opened) => Ok(DiscSession::Iso(opened.iso)),
        Err(BurnError::NoIsoSession) => match udf::open_udf_session(source, cancel) {
            Ok(volume) => Ok(DiscSession::Udf(volume)),
            Err(BurnError::NoIsoSession) => Err(BurnError::NoIsoSession),
            Err(other) => Err(other),
        },
        Err(other) => Err(other),
    }
}

/// 原生读侧后端：接在 [`ReadBackend`] 接缝上（ADR-0018）。
pub(crate) struct NativeRead;

impl ReadBackend for NativeRead {
    fn read_volume_id(&self, source: &str) -> Result<String, BurnError> {
        let cancel = CancelToken::default();
        match open_session(Path::new(source), &cancel)? {
            DiscSession::Iso(iso) => volume_id(&iso, &cancel),
            DiscSession::Udf(volume) => udf::volume_id(&volume),
        }
    }

    fn iso_session_state(&self, source: &str) -> Result<IsoSessionState, BurnError> {
        // UDF 不进这个判断：门禁问的是“盘上有没有可用的 ISO 9660 会话”。
        match open_readable_session(Path::new(source), &CancelToken::default()) {
            Ok(opened) => Ok(match opened.fallback {
                Some(fallback) => IsoSessionState::Damaged(fallback),
                None => IsoSessionState::Usable,
            }),
            // 没有可用的 ISO 9660 会话（空白盘、纯 UDF 盘、音频轨）按“不可用”
            // 回答，与 xorriso 路径识别兜底空镜像的口径一致，结构损坏等其它错误
            // 原样上抛。
            Err(BurnError::NoIsoSession) => Ok(IsoSessionState::Unusable),
            Err(other) => Err(other),
        }
    }

    fn list_tree(&self, source: &str) -> Result<Vec<DiscEntry>, BurnError> {
        let cancel = CancelToken::default();
        match open_session(Path::new(source), &cancel)? {
            DiscSession::Iso(iso) => list_iso_tree(&iso, &cancel),
            DiscSession::Udf(volume) => udf::list_tree(&volume, &cancel),
        }
    }

    fn extract_tree(
        &self,
        source: &Path,
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        match open_session(source, cancel)? {
            DiscSession::Iso(iso) => {
                let root = iso.root_dir();
                let joliet = is_joliet_root(&root.entry_type());
                extract_dir(&iso, root.dir_ref(), joliet, dest, cancel)
            }
            DiscSession::Udf(volume) => udf::extract_tree(&volume, dest, cancel),
        }
    }

    fn extract_paths(
        &self,
        source: &str,
        paths: &[String],
        dest: &Path,
        cancel: &CancelToken,
    ) -> Result<(), BurnError> {
        match open_session(Path::new(source), cancel)? {
            DiscSession::Iso(iso) => extract_iso_paths(&iso, paths, dest, cancel),
            DiscSession::Udf(volume) => udf::extract_paths(&volume, paths, dest, cancel),
        }
    }
}

/// ISO 分支的列举。
fn list_iso_tree(
    iso: &IsoImage<SessionSource>,
    cancel: &CancelToken,
) -> Result<Vec<DiscEntry>, BurnError> {
    let root = iso.root_dir();
    let joliet = is_joliet_root(&root.entry_type());
    let mut entries = Vec::new();
    walk_list(iso, root.dir_ref(), "", joliet, &mut entries, cancel)?;
    Ok(entries)
}

/// ISO 分支的按路径抽取（UDF 分支在 [`udf::extract_paths`]）。
fn extract_iso_paths(
    iso: &IsoImage<SessionSource>,
    paths: &[String],
    dest: &Path,
    cancel: &CancelToken,
) -> Result<(), BurnError> {
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
            let dir = entry.as_dir_ref(iso).map_err(|e| hadris_error(e, cancel))?;
            extract_dir(iso, dir, joliet, &target, cancel)?;
        } else {
            let data = iso.read_file(&entry).map_err(|e| hadris_error(e, cancel))?;
            std::fs::write(&target, &data)?;
        }
    }
    Ok(())
}

/// 候选区段起点，新到旧。设备路径用 READ TOC：Format 1 的末区段起点加 Format 0
/// 的轨道起点（去掉驱动器报的导出区），去重后从新到旧。镜像文件只有一个起点 0。
///
/// 每个区段的首条轨道起点就是该区段起点，所以轨道列表是区段起点集合的超集；同一
/// 区段里的后续轨道会被验证步骤淘汰（它们的起点加 16 块处没有 ISO 描述符区）。
pub(crate) fn session_candidates(mmc: &mut MmcDevice) -> Result<Vec<u32>, BurnError> {
    let mut candidates = Vec::new();
    if let Some(session) = mmc.read_toc_session_info()? {
        candidates.push(session.last_session_start);
    }
    // 保守开关：只认末区段，关掉候选回退（issue #40 的待决策 4）。读盘与追加共用
    // 这一个枚举，置位后两边都退回改动前的行为，便于对照与止损。
    if last_session_only() {
        return Ok(candidates);
    }
    // 轨道列表读不到不算致命：正常盘上 Format 1 的起点就够，退回单个候选。
    if let Ok(tracks) = mmc.read_toc_tracks() {
        for track in tracks {
            if !track.is_lead_out {
                candidates.push(track.start_lba);
            }
        }
    }
    candidates.sort_unstable_by(|left, right| right.cmp(left));
    candidates.dedup();
    Ok(candidates)
}

/// `OPTIBURN_LAST_SESSION_ONLY` 置 1 时关掉候选回退（issue #40 的待决策 4）。
fn last_session_only() -> bool {
    last_session_only_value(std::env::var("OPTIBURN_LAST_SESSION_ONLY").ok().as_deref())
}

/// 开关的取值判定：空值、`0`、`off` 都算关。抽出来是为了能不碰进程环境测。
fn last_session_only_value(value: Option<&str>) -> bool {
    !matches!(value, None | Some("") | Some("0") | Some("off"))
}

/// 校验一个候选区段能不能用：整棵目录树都要走得通（每条记录能解析、目录数据
/// 都读得到）。残片的常见形态就是目录结构写到一半，走不通即淘汰。
///
/// 按 issue #40 的决策只走目录树，不逐文件读一遍；设备路径上源长度未知，能读到
/// 哪里由驱动器决定，树的遍历本身就会把越界读暴露成错误。
fn validate_session(iso: &IsoImage<SessionSource>, cancel: &CancelToken) -> Result<(), BurnError> {
    let root = iso.root_dir();
    let joliet = is_joliet_root(&root.entry_type());
    let mut entries = Vec::new();
    walk_list(iso, root.dir_ref(), "", joliet, &mut entries, cancel)?;
    Ok(())
}

/// 打开“最新一个通过验证的区段”。正常盘上就是 READ TOC Format 1 报的末区段；
/// 末区段损坏（固件把残片登记成区段，或目录结构不完整）时按候选列表回退到最新的
/// 可用区段。
///
/// 全部候选都不可用时返回**最新那个候选**的错误，让文案与今天一致（读盘报
/// `NoIsoSession`，设备错误原样上抛）。
fn open_readable_session(source: &Path, cancel: &CancelToken) -> Result<OpenedSession, BurnError> {
    match classify_source(source) {
        SourceKind::Image(path) => {
            let file = File::open(&path)?;
            let len = file.metadata()?.len();
            if len < SECTOR_BYTES as u64 * 17 {
                return Err(BurnError::NoIsoSession);
            }
            let iso = open_session_view(FileBlocks(file), 0, len, cancel)?;
            validate_session(&iso, cancel)?;
            Ok(OpenedSession {
                iso,
                fallback: None,
            })
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
            open_readable_session_on_device(mmc, cancel)
        }
    }
}

/// 设备分支的主体：候选枚举加逐个尝试。抽出来是为了能在没有光驱的机器上用假
/// 传输层测（真机路径只有 [`open_transport_and_readable_session`] 那一层包装）。
pub(crate) fn open_readable_session_on_device(
    mmc: MmcDevice,
    cancel: &CancelToken,
) -> Result<OpenedSession, BurnError> {
    let mut mmc = mmc;
    let candidates = session_candidates(&mut mmc)?;
    if candidates.is_empty() {
        return Err(BurnError::NoIsoSession);
    }
    let total = candidates.len();
    // 候选逐个尝试，设备只有一个：用共享块源让每个视图都能读同一台设备。
    let shared = Rc::new(RefCell::new(
        Box::new(DiscBlocks(mmc)) as Box<dyn BlockSource>
    ));
    let mut newest_error = None;
    for (index, base) in candidates.iter().enumerate() {
        let attempt = open_session_view(SharedBlocks(Rc::clone(&shared)), *base, u64::MAX, cancel)
            .and_then(|iso| validate_session(&iso, cancel).map(|()| iso));
        match attempt {
            Ok(iso) => {
                let fallback = (index > 0).then_some(SessionFallback {
                    skipped: index,
                    ordinal: total - index,
                    candidates: total,
                    session_start: *base,
                });
                return Ok(OpenedSession { iso, fallback });
            }
            Err(error) => {
                if newest_error.is_none() {
                    newest_error = Some(error);
                }
            }
        }
    }
    Err(newest_error.unwrap_or(BurnError::NoIsoSession))
}

/// 打开可读区段的结果：视图加“是否回退过”的信息（回退过时调用方要告知用户，被
/// 跳过区段里的文件不在这个视图里）。
pub(crate) struct OpenedSession {
    pub iso: IsoImage<SessionSource>,
    pub fallback: Option<SessionFallback>,
}

/// 在起点 `base` 的区段上探测地址约定并打开 ISO 视图。`len_hint` 只在
/// 调用方已知源长度时给出（镜像文件），设备路径的长度从 PVD 的卷空间大小取。
pub(crate) fn open_session_view<B: BlockSource + 'static>(
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

/// 探测区段的地址约定：读根目录 extent 所在块，看它的首条目录记录（“.”）
/// 是否自引用。相对约定的记录里写的是相对块号（等于 E），绝对约定里写的是盘上
/// LBA（同样等于 E，因为两种约定下这个字段都指向根目录自身所在的源内块）。
/// 判定用“读到的块号 == 记录声称的块号”，按相对、绝对的顺序尝试。相对候选的
/// 读取失败（绝对约定的盘上 base+E 可能越过可读范围，驱动器报错）不算结论，
/// 换绝对候选再判。增长模式读旧区段时也用它（[`crate::grow`]）。
pub(crate) fn probe_address_mode(
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
pub(crate) fn is_joliet_root(entry_type: &EntryType) -> bool {
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

/// 深度优先列目录树，顺序即目录记录顺序（xorriso 的 -find 也是树序）。增长模式的
/// 测试也用它读回生成的会话（[`crate::grow`]）。
pub(crate) fn walk_list(
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
