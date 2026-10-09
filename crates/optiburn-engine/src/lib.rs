//! 刻录引擎：把镜像写到盘上。
//!
//! v0 只有一种引擎：调用 `xorriso -as cdrecord` 子进程（ADR-0004）。原生 MMC 写入
//! 引擎（RESERVE TRACK / WRITE(10) / CLOSE TRACK）落地后接在同一个 [`BurnEngine`]
//! 接缝上，不需要空壳占位。
//!
//! 除写盘外还有回读用的小工具：读盘上卷标、把镜像或设备的目录树抽到本地，以及
//! 本地目录树的内容对比（回读校验，见 ADR-0010）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod readback;
mod verify;
mod xorriso;

pub use readback::{
    DiscEntry, extract_paths, extract_tree, last_session_is_iso, list_tree, read_volume_id,
};
pub use verify::compare_trees;
pub use xorriso::{BurnFailure, XorrisoEngine, grow};

/// 依赖的可执行文件名。
pub(crate) const XORRISO: &str = "xorriso";
/// 子进程失败时保留多少行 stderr 作为摘要。
pub(crate) const TAIL_LINES: usize = 10;

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
    /// 盘内路径不安全（盘符前缀、`..` 或 Windows 分隔符），拒绝抽取。
    #[error("unsafe path in image: {0}")]
    UnsafePath(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
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
