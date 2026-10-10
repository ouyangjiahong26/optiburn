//! 刻录引擎：把镜像写到盘上。
//!
//! 两种引擎接在同一个 [`BurnEngine`] 接缝上：
//!
//! - [`NativeEngine`]：自己发 MMC 命令（MODE SELECT / WRITE(10) / SYNCHRONIZE CACHE /
//!   CLOSE TRACK/SESSION），
//!   不依赖外部程序，Windows 上唯一可用的刻录路径（ADR-0017）。
//! - [`XorrisoEngine`]：调用 `xorriso -as cdrecord` 子进程（ADR-0004）。
//!
//! 除写盘外还有读侧：读盘上卷标、列目录树、抽取到本地，以及本地目录树的内容
//! 对比（回读校验，见 ADR-0010）。读侧同样有两个后端（xorriso 子进程与原生
//! MMC 加 ISO 9660 解析，ADR-0018），Windows 走原生、Linux 维持 xorriso。
//!
//! 增长模式（`append`/追加页）同样分平台：Windows 用原生引擎读旧区段、生成
//! 绝对地址约定的新区段（ADR-0020），Linux 维持 xorriso 的增长模式。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod disc_read;
mod grow;
mod native;
mod readback;
mod verify;
mod xorriso;

pub use native::NativeEngine;
pub use readback::{
    DiscEntry, extract_paths, extract_tree, iso_session_state, list_tree, read_volume_id,
};
pub use verify::compare_trees;
pub use xorriso::{BurnFailure, XorrisoEngine};

/// 把目录追加到盘上（增长模式）：读出盘上末区段的目录树，把源目录并进去后作为
/// 新区段提交。
///
/// Windows 走原生引擎（自己读旧区段、生成新区段，见 ADR-0020），Linux 维持
/// xorriso 增长模式（ADR-0006）。分派规则与写侧、读侧同理由：哪条路在平台上
/// 走得通，不是设备语义差异。
pub fn grow(
    job: &GrowJob,
    progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<(), BurnError> {
    if cfg!(windows) {
        native::grow(job, progress, cancel)
    } else {
        xorriso::grow(job, progress, cancel)
    }
}

/// 尝试修复被中断刻录留下的损坏轨道与区段：按 libburn 的 `burn_disc_close_damaged`
/// 的顺序（CD 与 DVD-R 族先下发写参数页再关区段，+R 族与 BD-R 关最后一条轨道），
/// 等价于 xorriso 的 `-close_damaged as_needed`（`force` 对应 `force`）。驱动器报
/// 损坏才动，`force` 时无条件尝试；命令与介质族不匹配时返回错误。
///
/// 只做修复与状态复核，不写数据；修复成功后 [`grow`] 就可能在这张盘上继续。
pub fn repair(device: &str, force: bool) -> Result<RepairOutcome, BurnError> {
    if cfg!(windows) {
        native::repair(device, force)
    } else {
        xorriso::repair(device, force)
    }
}

/// 追加会话的尺寸预演：返回即将写入的新区段字节数，供调用方的写前容量门禁
/// （ADR-0019）。不写盘，但会读盘片状态与旧区段目录树。
pub fn grow_size(job: &GrowJob) -> Result<u64, BurnError> {
    if cfg!(windows) {
        native::grow_size(job)
    } else {
        xorriso::grow_size(job)
    }
}

/// 依赖的可执行文件名。
pub(crate) const XORRISO: &str = "xorriso";
/// 子进程失败时保留多少行 stderr 作为摘要。
pub(crate) const TAIL_LINES: usize = 10;
/// 容量门禁的区段开销余量（ADR-0019）：待写入量（[`grow_size`] 的预演字节数
/// 或镜像大小）只算数据区，不含区段 lead-in/lead-out 与链接区。CD 每区段最大约
/// 15 MB，DVD/BD 约 2 MB，取覆盖最坏情形的 16 MB。CLI 与 GUI 的写前容量门禁
/// 共用这一个常量，避免两端口径漂移。
pub const SESSION_OVERHEAD: u64 = 16 * 1024 * 1024;

/// 一次镜像刻录任务的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnJob {
    /// 待写入的镜像文件，例如 `.iso`。
    pub image: PathBuf,
    /// 目标设备：Linux 形如 `/dev/sr0`，Windows 形如 `E:`。
    pub device: String,
    /// 写入倍速。`None` 表示交给驱动自选。
    pub speed: Option<u32>,
    /// 是否以多区段方式追加（`-multi`）。
    pub multi: bool,
}

/// 一次目录追加（增长模式）任务的输入。
///
/// 增长模式不经过镜像文件：xorriso 读出盘上已有区段的目录树，把源目录内容并进
/// 去后作为新区段提交，因此旧文件仍可见。hadris 只能从零建整盘镜像，区段合并
/// 只能落在引擎侧（见 ADR-0006）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrowJob {
    /// 源目录，其内容成为盘上根目录。
    pub src: PathBuf,
    /// 目标设备：Linux 形如 `/dev/sr0`，Windows 形如 `E:`。
    pub device: String,
    /// 写入倍速。`None` 表示交给驱动自选。
    pub speed: Option<u32>,
    /// 新区段的卷标。
    pub volume_id: String,
    /// 提交后把盘标记为不可追加（封盘）。
    pub close_disc: bool,
    /// 允许在末区段损坏的盘上追加，回退到更早的可读区段（ADR-0022）。
    ///
    /// 置位前调用方必须把 [`SessionFallback`] 的内容告知用户：被跳过区段里的文件
    /// 不在新会话的目录树里，等于从可见视图消失。没有这个授权时引擎按
    /// [`BurnError::DamagedLastSession`] 拒绝。
    pub allow_damaged_last_session: bool,
}

/// 末区段损坏时回退到更早区段的信息（读取与追加共用，ADR-0022）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionFallback {
    /// 被跳过的候选个数（0 表示末区段本身可用）。
    pub skipped: usize,
    /// 选中区段在候选里的序号，从旧到新数，1 起。
    pub ordinal: usize,
    /// 参与枚举的候选总数。
    pub candidates: usize,
    /// 选中区段的起点（盘级块号）。
    pub session_start: u32,
}

/// 修复尝试的结果（`optiburn repair`，对齐 xorriso 的 `-close_damaged`，ADR-0022）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairOutcome {
    /// 驱动器是否把下一轨道报成损坏（MMC 的 Damage 位）。
    pub damaged: bool,
    /// 是否真的执行了修复（未报损坏且没强制时不执行）。
    pub attempted: bool,
    /// 修复后是否拿到可用的可写地址。
    pub writable: bool,
    /// 修复后的下一个可写地址。
    pub next_writable_address: Option<u32>,
    /// 修复后的剩余可写字节数（驱动器报剩余块数时）。
    pub free_bytes: Option<u64>,
    /// 修复命令自身的报错（驱动器拒绝修复时），执行成功是 `None`。
    pub error: Option<String>,
}

/// 盘上 ISO 9660 会话的可用状态，追加门禁据此分流（ADR-0022）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoSessionState {
    /// 末区段就是可读的 ISO 9660 会话。
    Usable,
    /// 末区段损坏，回退到更早的会话；追加会跳过被跳过区段里的文件，需要确认。
    Damaged(SessionFallback),
    /// 盘上没有被支持的 ISO 9660 会话（空白盘、纯 UDF 盘、音频盘）。
    Unusable,
}

#[derive(Debug, thiserror::Error)]
pub enum BurnError {
    #[error("missing tool: {0}")]
    MissingTool(String),
    #[error("burn failed: {0}")]
    Failed(String),
    #[error("burn cancelled")]
    Cancelled,
    /// 盘上最后一区段不是 ISO 9660（例如 UDF 盘），相关读取与续写不可用。
    #[error("no ISO 9660 image at the last session")]
    NoIsoSession,
    /// 读侧子进程失败（列目录、读卷标等），与刻录失败区分开，动作语境由调用方给出。
    #[error("{0}")]
    ReadFailed(String),
    /// 盘内路径不安全（盘符前缀、`..` 或 Windows 分隔符），拒绝抽取。
    #[error("unsafe path in image: {0}")]
    UnsafePath(String),
    /// 原生引擎的 MMC 命令失败（传输层错误或命令级失败），与子进程引擎的错误分开。
    #[error("MMC: {0}")]
    Mmc(#[from] optiburn_mmc::MmcError),
    /// 原生引擎做不到这次请求，原因见 [`NativeGap`]，文案由 CLI 与 GUI 分别给出。
    #[error("native engine gap: {0}")]
    NativeGap(NativeGap),
    /// 盘上剩余空间放不下待写入的新区段（原生增长模式的门禁，ADR-0020）。
    #[error("not enough space for the new session: needs {needed} bytes, {free} free")]
    NotEnoughSpace { needed: u64, free: u64 },
    /// 盘上旧区段的形状不支持嫁接式增长（无 Joliet、启动记录、多 extent 文件等）。
    #[error("cannot grow this disc with the native engine: {0}")]
    GrowUnsupported(String),
    /// 末区段损坏，回退到更早区段前需要用户确认（ADR-0022）。
    #[error(
        "the last session is damaged: skipped {} candidates, the readable session starts at LBA {}",
        .fallback.skipped,
        .fallback.session_start
    )]
    DamagedLastSession { fallback: SessionFallback },
    /// 盘上没有可用的可写地址（NWA_V 清零，或 NWA 不落在末区段之后），不能续写。
    /// `damaged` 区分两种形态：驱动器认了损坏轨道（libburn 的 "Damaged, not closed
    /// and not writable"）与单纯的地址不可用（"No Next-Writable-Address"）。
    #[error(
        "no usable next writable address (last session at LBA {last_session_start}, damaged {damaged})"
    )]
    WriteAddressUnknown {
        last_session_start: u32,
        damaged: bool,
    },
    /// 追加的目录与盘上已有内容冲突（同名文件对目录、命名空间内重名）。
    #[error("growth content conflicts with the disc: {0}")]
    GrowConflict(String),
    /// 盘上最后一区段是 UDF，但用了 hadris-udf 读不了的结构（VAT、元数据分区等）。
    #[error("unsupported UDF structure: {0}")]
    UnsupportedUdf(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// 原生引擎的能力缺口：结构与理由分开，界面层才能各给母语文案（ADR-0017）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeGap {
    #[error("setting the write speed (SET CD SPEED) is not implemented yet")]
    WriteSpeed,
    #[error("the image is empty")]
    EmptyImage,
    #[error("the image does not fit on the media")]
    ImageTooLarge,
    #[error("the image exceeds the addressable 2048-byte blocks")]
    ImageBeyondAddressRange,
    #[error("media profile {0:#06x} is not in the supported list")]
    UnsupportedProfile(u16),
    #[error("the disc is finalized")]
    FinalizedDisc,
    #[error("growing a rewritable disc is not supported by the native engine")]
    GrowthOnRewritable,
    #[error("the source directory is empty")]
    EmptyGrowSource,
}

impl BurnError {
    /// 缺外部工具时给用户的中文说明，CLI 与 GUI 共用一份，避免两端各写一份后
    /// 口径漂移。安装指引只在这里和 GUI 的英文镜像里维护，`MissingTool` 本身
    /// 只带工具名，不再把某一种系统的安装命令编进跨平台的错误里。
    ///
    /// Windows 上写 MSYS2 的构建不可用而不是给出安装命令：实测（1.5.8.pl02，
    /// 2026-10-10 于 USB 光驱）该构建不含 MMC 传输层，`-devices` 报
    /// `No MMC transport adapter is present. Running on sg-dummy.c.`，设备参数会
    /// 落进 libburn 的 stdio 伪设备。装了也读不了盘、刻不了录。
    pub fn missing_tool_user_text(tool: &str) -> String {
        format!(
            "缺少 {tool}，读取盘片与刻录都依赖它。Linux 用发行版的包管理器安装（Debian/Ubuntu 是 sudo apt install {tool}）。Windows 上 MSYS2 的构建不含光驱访问，装了也无法读盘与刻录。"
        )
    }
}

/// 协作式取消令牌：取消方置位，引擎在进度循环里检查并停掉子进程。
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 置位取消。Relaxed 就够：这一位只承载“要不要停”，晚一行输出被看到
    /// 无妨，也不需要与其它内存访问建立先后关系。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// 一种把镜像写到盘上的方式。
pub trait BurnEngine {
    /// 引擎名，用于 CLI 的 `--engine` 取值与日志。
    fn name(&self) -> &'static str;

    /// 执行刻录。`progress` 收到 0.0–1.0 的进度回调；`cancel` 在进度循环里被
    /// 检查，置位后子进程被停掉并返回 `BurnError::Cancelled`，除非子进程已自然
    /// 写完退出——那时盘已写完，迟到取消不算数。
    fn burn(
        &self,
        job: &BurnJob,
        progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<(), BurnError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺工具文案钉住两条事实：Linux 给得出安装命令，Windows 不给（MSYS2 的构建
    /// 不含光驱访问，写成安装指引会把用户引到死路）。
    #[test]
    fn missing_tool_text_names_the_linux_route_only() {
        let text = BurnError::missing_tool_user_text("xorriso");
        assert!(text.contains("xorriso"), "{text}");
        assert!(text.contains("sudo apt install xorriso"), "{text}");
        assert!(text.contains("MSYS2"), "{text}");
        assert!(!text.contains("pacman"), "{text}");
    }
}
