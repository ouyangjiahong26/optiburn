//! 任务状态：按设备键控的任务槽。同一设备同时最多一个任务，不同设备并行（issue #50）。
//! 镜像制作没有设备，用固定伪键 "build" 占一个槽：Windows 设备串是盘符加冒号、
//! Linux 是 `/dev/*`，都不会撞上这个词。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use optiburn_engine::CancelToken;
use tauri::async_runtime::JoinHandle;

/// 正在运行的任务类别，随进度与完成事件发给前端。
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    Build,
    Burn,
    Append,
    Verify,
    /// 把盘上文件复制到系统剪贴板：与写盘同占任务槽，二者互斥。
    Copy,
    /// 抢救未关闭轨道（刻录中断的那一次写入）里的数据：同为任务槽互斥项。
    Salvage,
}

/// 任务槽占用冲突：槽键即设备串（build 用 "build"）。
#[derive(Debug)]
pub struct BusySlot(pub String);

/// 登记成功后交给命令层的手柄：取消令牌加事件配对序号。
pub struct JobHandle {
    pub cancel: CancelToken,
    pub seq: u64,
}

/// 一次阻塞任务的运行时信息。
pub struct RunningJob {
    pub kind: JobKind,
    pub seq: u64,
    pub cancel: CancelToken,
    /// 工作句柄：confirm_close 要等 kill 生效后再退出进程。
    /// spawn 之后才拿得到句柄，所以允许短暂为 None。
    pub join: Option<JoinHandle<()>>,
}

/// 全局任务槽：按设备键控。同一设备同时最多一个任务，不同设备并行（issue #50）。
#[derive(Default)]
pub struct JobState {
    slots: Mutex<HashMap<String, RunningJob>>,
    next_seq: AtomicU64,
}

impl JobState {
    /// 占住一个槽：同槽已有任务时拒绝。序号从 1 起自增，事件靠它把进度与完成配对，
    /// 收尾只认自己的序号，不会清掉接棒的下一个任务。
    pub fn begin(&self, slot: &str, kind: JobKind) -> Result<JobHandle, BusySlot> {
        let mut slots = self.slots.lock().expect("job state mutex");
        if slots.contains_key(slot) {
            return Err(BusySlot(slot.to_string()));
        }
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = CancelToken::new();
        slots.insert(
            slot.to_string(),
            RunningJob {
                kind,
                seq,
                cancel: cancel.clone(),
                join: None,
            },
        );
        Ok(JobHandle { cancel, seq })
    }

    /// spawn_blocking 之后补记工作句柄，供 confirm_close 等待退出。
    /// 任务可能已结束（槽被清），静默丢弃。
    pub fn set_join(&self, slot: &str, seq: u64, join: JoinHandle<()>) {
        let mut slots = self.slots.lock().expect("job state mutex");
        if let Some(job) = slots.get_mut(slot)
            && job.seq == seq
        {
            job.join = Some(join);
        }
    }

    /// 释放一个槽：只认序号匹配的收尾，旧序号不清接棒的新任务。
    pub fn finish(&self, slot: &str, seq: u64) {
        let mut slots = self.slots.lock().expect("job state mutex");
        if slots.get(slot).is_some_and(|job| job.seq == seq) {
            slots.remove(slot);
        }
    }

    /// 取消一台设备上的任务：命中返回真，槽里没有任务返回假。
    pub fn cancel(&self, slot: &str) -> bool {
        match self.slots.lock().expect("job state mutex").get(slot) {
            Some(job) => {
                job.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// 关窗收尾：对全部任务置位取消、清空映射、交回句柄由调用方逐个等待。
    pub fn cancel_all_and_drain(&self) -> Vec<JoinHandle<()>> {
        let mut slots = self.slots.lock().expect("job state mutex");
        slots
            .drain()
            .filter_map(|(slot, job)| {
                // 关窗把还在跑的任务拦下来，日志留一份设备与类别的记录，便于排查
                // 关窗为什么在等。
                eprintln!("optiburn: 关窗取消任务：{slot}（{:?}）", job.kind);
                job.cancel.cancel();
                job.join
            })
            .collect()
    }

    /// 是否还有任务在跑：关窗守卫据此决定拦不拦。
    pub fn is_empty(&self) -> bool {
        self.slots.lock().expect("job state mutex").is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_devices_register_in_parallel() {
        let state = JobState::default();
        let first = state.begin("D:", JobKind::Burn).unwrap();
        let second = state.begin("E:", JobKind::Append).unwrap();
        assert!(second.seq > first.seq);
    }

    #[test]
    fn same_device_second_registration_is_rejected() {
        let state = JobState::default();
        state.begin("D:", JobKind::Burn).unwrap();
        match state.begin("D:", JobKind::Append) {
            Err(BusySlot(slot)) => assert_eq!(slot, "D:"),
            Ok(_) => panic!("second registration on the same device must be rejected"),
        }
    }

    #[test]
    fn finish_by_seq_frees_only_that_slot() {
        let state = JobState::default();
        let d = state.begin("D:", JobKind::Burn).unwrap();
        state.begin("E:", JobKind::Append).unwrap();
        state.finish("D:", d.seq);
        // D: 释放后可再登记（拿到新序号），E: 不受影响仍占用。
        let again = state.begin("D:", JobKind::Verify).unwrap();
        assert!(again.seq > d.seq);
        assert!(matches!(
            state.begin("E:", JobKind::Verify),
            Err(BusySlot(slot)) if slot == "E:"
        ));
    }

    #[test]
    fn stale_finish_does_not_clear_the_new_job() {
        let state = JobState::default();
        let stale = state.begin("D:", JobKind::Burn).unwrap();
        state.finish("D:", stale.seq);
        let current = state.begin("D:", JobKind::Append).unwrap();
        // 旧任务的迟到收尾不能清掉接棒任务。
        state.finish("D:", stale.seq);
        assert!(matches!(
            state.begin("D:", JobKind::Verify),
            Err(BusySlot(slot)) if slot == "D:"
        ));
        state.finish("D:", current.seq);
        assert!(state.is_empty());
    }

    #[test]
    fn cancel_targets_only_one_device() {
        let state = JobState::default();
        let d = state.begin("D:", JobKind::Burn).unwrap();
        let e = state.begin("E:", JobKind::Append).unwrap();
        assert!(state.cancel("D:"));
        assert!(d.cancel.is_cancelled());
        assert!(!e.cancel.is_cancelled());
        assert!(!state.cancel("F:"));
    }

    #[test]
    fn cancel_all_drains_every_slot() {
        let state = JobState::default();
        let d = state.begin("D:", JobKind::Burn).unwrap();
        let e = state.begin("E:", JobKind::Append).unwrap();
        state.set_join("D:", d.seq, tauri::async_runtime::spawn(async {}));
        state.set_join("E:", e.seq, tauri::async_runtime::spawn(async {}));
        let joins = state.cancel_all_and_drain();
        assert_eq!(joins.len(), 2);
        assert!(state.is_empty());
        assert!(d.cancel.is_cancelled());
        assert!(e.cancel.is_cancelled());
    }
}
