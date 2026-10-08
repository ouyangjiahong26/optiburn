//! 任务状态：同一时刻最多一个阻塞任务，持有取消令牌与工作句柄。

use std::sync::Mutex;

use optiburn_engine::CancelToken;
use tauri::async_runtime::JoinHandle;

/// 正在运行的任务类别，随进度与完成事件发给前端。
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    Build,
    Burn,
    Append,
}

/// 一次阻塞任务的运行时信息。
pub struct RunningJob {
    pub cancel: CancelToken,
    /// 工作句柄：confirm_close 要等 kill 生效后再退出进程。
    /// spawn 之后才拿得到句柄，所以允许短暂为 None。
    pub join: Option<JoinHandle<()>>,
}

/// 全局任务槽。
#[derive(Default)]
pub struct JobState(pub Mutex<Option<RunningJob>>);
