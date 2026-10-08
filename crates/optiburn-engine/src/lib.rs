//! 刻录引擎：把镜像写到盘上。
//!
//! v0 只有一种引擎：调用 `xorriso -as cdrecord` 子进程（ADR-0004）。原生 MMC 写入
//! 引擎（RESERVE TRACK / WRITE(10) / CLOSE TRACK）落地后接在同一个 [`BurnEngine`]
//! 接缝上，不需要空壳占位。

use std::path::PathBuf;

mod xorriso;

pub use xorriso::{XorrisoEngine, grow};

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
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// 一种把镜像写到盘上的方式。
pub trait BurnEngine {
    /// 引擎名，用于 CLI 的 `--engine` 取值与日志。
    fn name(&self) -> &'static str;

    /// 执行刻录。`progress` 收到 0.0–1.0 的进度回调。
    fn burn(&self, job: &BurnJob, progress: &mut dyn FnMut(f32)) -> Result<(), BurnError>;
}
