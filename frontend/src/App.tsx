// 侧栏导航 + 页面切换 + 全局任务状态：进度、结果、取消与退出确认都在这里。
import { useCallback, useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Disc3, FileArchive, Flame, FolderPlus } from "lucide-react";
import { cancelJob, confirmClose, onCloseBlocked, onJobDone, onJobProgress } from "./api";
import type { ActiveJob, JobKind, JobResult } from "./types";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { ProgressBar } from "./components/ProgressBar";
import { AppendPage } from "./pages/AppendPage";
import { BuildPage } from "./pages/BuildPage";
import { BurnPage } from "./pages/BurnPage";
import { DevicesPage } from "./pages/DevicesPage";

type PageId = "devices" | "build" | "burn" | "append";

const NAV_ITEMS: { id: PageId; label: string; icon: typeof Disc3 }[] = [
  { id: "devices", label: "设备", icon: Disc3 },
  { id: "build", label: "制作镜像", icon: FileArchive },
  { id: "burn", label: "刻录", icon: Flame },
  { id: "append", label: "追加", icon: FolderPlus },
];

// 制作镜像没有进度回调，只显示动作本身；其余两种有百分比就带上。
function jobStatusText(job: ActiveJob): string {
  if (job.kind === "build") {
    return "正在制作镜像……";
  }
  const action = job.kind === "burn" ? "刻录" : "追加";
  return job.fraction === null
    ? `正在${action}……`
    : `正在${action}，已写入 ${Math.round(job.fraction * 100)}%`;
}

export function App() {
  const [page, setPage] = useState<PageId>("devices");
  const [job, setJob] = useState<ActiveJob | null>(null);
  const [result, setResult] = useState<JobResult | null>(null);
  const [refreshSignal, setRefreshSignal] = useState(0);
  const [cancelDialogOpen, setCancelDialogOpen] = useState(false);
  const [closeDialogOpen, setCloseDialogOpen] = useState(false);
  const seqRef = useRef(0);
  const pageRef = useRef(page);
  pageRef.current = page;

  // 提交即锁定：等第一个进度或完成事件接管，避免窗口期里再次提交或切页。
  const startJob = useCallback((kind: JobKind) => {
    setResult(null);
    setJob((current) => current ?? { kind, fraction: null });
  }, []);

  // invoke 被立即拒绝时不会有 job-done 事件，用这里解锁并展示原因；
  // 若任务已由事件接管（kind 不匹配），这里不动任何状态。
  const abortJob = useCallback((kind: JobKind, message?: string) => {
    setJob((current) =>
      current !== null && current.kind === kind ? null : current,
    );
    if (message !== undefined) {
      seqRef.current += 1;
      setResult({ kind, outcome: "failed", message, seq: seqRef.current });
    }
  }, []);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;
    // StrictMode 挂载两次：订阅真正建立后才登记清理函数，卸载先行则直接反订阅。
    void Promise.all([
      onJobProgress((progress) => {
        setJob({ kind: progress.kind, fraction: progress.fraction });
      }),
      onJobDone((done) => {
        setJob(null);
        seqRef.current += 1;
        setResult({ ...done, seq: seqRef.current });
        if (pageRef.current === "devices") {
          setRefreshSignal((signal) => signal + 1);
        }
      }),
      onCloseBlocked(() => {
        setCloseDialogOpen(true);
      }),
    ]).then((fns) => {
      if (disposed) {
        fns.forEach((fn) => fn());
      } else {
        unlisteners.push(...fns);
      }
    });
    return () => {
      disposed = true;
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  const locked = job !== null;

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">OptiBurn</div>
        <nav className="nav">
          {NAV_ITEMS.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`nav-item${page === item.id ? " active" : ""}`}
              disabled={locked}
              onClick={() => setPage(item.id)}
            >
              <item.icon size={15} />
              {item.label}
            </button>
          ))}
        </nav>
      </aside>
      <main className="content">
        {job !== null && (
          <section className="job-bar">
            <ProgressBar fraction={job.fraction} />
            <div className="job-status-row">
              <span className="job-status">{jobStatusText(job)}</span>
              {(job.kind === "burn" || job.kind === "append") && (
                <button
                  type="button"
                  className="btn btn-danger"
                  onClick={() => setCancelDialogOpen(true)}
                >
                  停止
                </button>
              )}
            </div>
          </section>
        )}
        {result !== null && (
          <div
            className={`result-banner${result.outcome === "done" ? "" : " warn"}`}
          >
            {result.message}
          </div>
        )}
        {page === "devices" && (
          <DevicesPage locked={locked} refreshSignal={refreshSignal} />
        )}
        {page === "build" && (
          <BuildPage locked={locked} onJobStart={startJob} onJobAbort={abortJob} />
        )}
        {page === "burn" && (
          <BurnPage
            locked={locked}
            result={result}
            onJobStart={startJob}
            onJobAbort={abortJob}
          />
        )}
        {page === "append" && (
          <AppendPage
            locked={locked}
            result={result}
            onJobStart={startJob}
            onJobAbort={abortJob}
          />
        )}
      </main>
      <ConfirmDialog
        open={cancelDialogOpen}
        title="停止任务"
        message="停止后盘片内容不完整，确认停止？"
        confirmText="停止"
        cancelText="继续"
        tone="danger"
        onConfirm={() => {
          setCancelDialogOpen(false);
          // 任务可能恰好刚结束：此时取消请求会被后端拒绝，而 job-done 已更新界面，忽略即可。
          cancelJob().catch(() => undefined);
        }}
        onCancel={() => setCancelDialogOpen(false)}
      />
      <ConfirmDialog
        open={closeDialogOpen}
        title="退出 OptiBurn"
        message="有任务正在进行。停止后盘片内容不完整，确认停止并退出？"
        confirmText="停止并退出"
        cancelText="继续运行"
        tone="danger"
        onConfirm={() => {
          setCloseDialogOpen(false);
          void confirmClose();
        }}
        onCancel={() => setCloseDialogOpen(false)}
      />
    </div>
  );
}
