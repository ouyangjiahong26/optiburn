//! 刻录引擎：把镜像写到盘上。
//!
//! 两种引擎接在同一个 [`BurnEngine`] 接缝上：
//!
//! - [`NativeEngine`]：自己发 MMC 命令（RESERVE TRACK / WRITE(10) / CLOSE TRACK），
//!   不依赖外部程序，Windows 上唯一可用的刻录路径（ADR-0017）。
//! - [`XorrisoEngine`]：调用 `xorriso -as cdrecord` 子进程（ADR-0004）。
//!
//! 除写盘外还有回读用的小工具：读盘上卷标、把镜像或设备的目录树抽到本地，以及
//! 本地目录树的内容对比（回读校验，见 ADR-0010）。回读侧目前只有 xorriso 一条路。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod native;
mod readback;
mod verify;
mod xorriso;

pub use native::NativeEngine;
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
