// 侧栏导航 + 页面切换 + 全局任务状态：进度、结果、取消与退出确认都在这里。
import { useCallback, useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Disc3, FileArchive, Flame, FolderPlus } from "lucide-react";
import { cancelJob, confirmClose, onCloseBlocked, onJobDone, onJobProgress } from "./api";
import type { ActiveJob, JobKind, JobResult } from "./types";
import { t } from "./i18n";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { ProgressBar } from "./components/ProgressBar";
import { AppendPage } from "./pages/AppendPage";
import { BuildPage } from "./pages/BuildPage";
import { BurnPage } from "./pages/BurnPage";
import { DevicesPage } from "./pages/DevicesPage";

type PageId = "devices" | "build" | "burn" | "append";

const NAV_ITEMS: { id: PageId; label: string; icon: typeof Disc3 }[] = [
  { id: "devices", label: t("Devices", "设备"), icon: Disc3 },
  { id: "append", label: t("Append", "追加"), icon: FolderPlus },
  { id: "burn", label: t("Burn", "刻录"), icon: Flame },
  { id: "build", label: t("Create Image", "制作镜像"), icon: FileArchive },
];

// 制作镜像没有进度回调，只显示动作本身；其余两种有百分比就带上。
function jobStatusText(job: ActiveJob): string {
  if (job.kind === "build") {
    return t("Creating image…", "正在制作镜像……");
  }
  if (job.kind === "verify") {
    return t("Verifying disc contents…", "正在校验盘上内容……");
  }
  if (job.kind === "copy") {
    return t("Copying files from disc…", "正在复制盘上文件……");
  }
  const en = job.kind === "burn" ? "Burning" : "Appending";
  const action = job.kind === "burn" ? "刻录" : "追加";
  return job.fraction === null
    ? t(`${en}…`, `正在${action}……`)
    : t(
        `${en}… ${Math.round(job.fraction * 100)}% written`,
        `正在${action}，已写入 ${Math.round(job.fraction * 100)}%`,
      );
}

export function App() {
  const [page, setPage] = useState<PageId>("devices");
  const [job, setJob] = useState<ActiveJob | null>(null);
  const [result, setResult] = useState<JobResult | null>(null);
  const [refreshSignal, setRefreshSignal] = useState(0);
  const [cancelDialogOpen, setCancelDialogOpen] = useState(false);
  const [closeDialogOpen, setCloseDialogOpen] = useState(false);
  const seqRef = useRef(0);

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
        // 页面常驻挂载，设备页自己决定展开中的清单要不要重读。
        setRefreshSignal((signal) => signal + 1);
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
  // 写盘类任务停止会留下不完整的盘，只读任务（复制、校验）不会，文案要分开。
  const discWriting = job !== null && (job.kind === "burn" || job.kind === "append");

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
              {(job.kind === "burn" ||
                job.kind === "append" ||
                job.kind === "copy") && (
                <button
                  type="button"
                  className="btn btn-danger"
                  onClick={() => setCancelDialogOpen(true)}
                >
                  {t("Stop", "停止")}
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
        {/* 四个页面全部保持挂载，切换只切可见性：读盘得到的清单、选择状态与正在
            进行的复制都不会因为切页而丢失。 */}
        <div className={page === "devices" ? undefined : "page-hidden"}>
          <DevicesPage
            locked={locked}
            refreshSignal={refreshSignal}
            active={page === "devices"}
            onJobStart={startJob}
            onJobAbort={abortJob}
          />
        </div>
        <div className={page === "build" ? undefined : "page-hidden"}>
          <BuildPage locked={locked} onJobStart={startJob} onJobAbort={abortJob} />
        </div>
        <div className={page === "burn" ? undefined : "page-hidden"}>
          <BurnPage
            locked={locked}
            result={result}
            active={page === "burn"}
            onJobStart={startJob}
            onJobAbort={abortJob}
          />
        </div>
        <div className={page === "append" ? undefined : "page-hidden"}>
          <AppendPage
            locked={locked}
            result={result}
            active={page === "append"}
            onJobStart={startJob}
            onJobAbort={abortJob}
          />
        </div>
      </main>
      <ConfirmDialog
        open={cancelDialogOpen}
        title={t("Stop task", "停止任务")}
        message={
          discWriting
            ? t(
                "Stopping now leaves the disc with incomplete contents. Stop anyway?",
                "停止后盘片内容不完整，确认停止？",
              )
            : t(
                "The task will not complete, but the disc contents are unaffected. Stop anyway?",
                "停止后本次任务不会完成，盘上内容不受影响。确认停止？",
              )
        }
        confirmText={t("Stop", "停止")}
        cancelText={t("Continue", "继续")}
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
        title={t("Quit OptiBurn", "退出 OptiBurn")}
        message={
          discWriting
            ? t(
                "A task is running. Stopping now leaves the disc with incomplete contents. Stop and quit anyway?",
                "有任务正在进行。停止后盘片内容不完整，确认停止并退出？",
              )
            : t(
                "A task is running. The task will not complete, but the disc contents are unaffected. Stop and quit anyway?",
                "有任务正在进行。停止后本次任务不会完成，盘上内容不受影响。确认停止并退出？",
              )
        }
        confirmText={t("Stop and quit", "停止并退出")}
        cancelText={t("Keep running", "继续运行")}
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
