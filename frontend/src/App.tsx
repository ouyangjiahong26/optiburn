// 侧栏导航 + 页面切换 + 全局任务状态：多设备并发下的当前任务清单、结果、逐任务
// 取消与退出确认都在这里（issue #50）。
import { useCallback, useEffect, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Disc3, FileArchive, Flame, FolderPlus } from "lucide-react";
import { cancelJob, confirmClose, onCloseBlocked, onJobDone, onJobProgress } from "./api";
import type { ActiveJob, JobKind, JobResult } from "./types";
import { nextLocalSeq } from "./hooks";
import { t } from "./i18n";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { ProgressBar } from "./components/ProgressBar";
import { UpdaterSection } from "./components/UpdaterSection";
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

// 制作镜像没有进度回调，只显示动作本身。其余两种有百分比就带上。
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
  if (job.kind === "salvage") {
    return t("Salvaging the unclosed track…", "正在抢救未关闭轨道里的数据……");
  }
  return job.fraction === null
    ? t(
        job.kind === "burn" ? "Burning…" : "Appending…",
        job.kind === "burn" ? "正在刻录……" : "正在追加……",
      )
    : t(
        job.kind === "burn"
          ? `Burning… ${Math.round(job.fraction * 100)}% written`
          : `Appending… ${Math.round(job.fraction * 100)}% written`,
        job.kind === "burn"
          ? `正在刻录，已写入 ${Math.round(job.fraction * 100)}%`
          : `正在追加，已写入 ${Math.round(job.fraction * 100)}%`,
      );
}

export function App() {
  const [page, setPage] = useState<PageId>("devices");
  const [jobs, setJobs] = useState<ActiveJob[]>([]);
  const [results, setResults] = useState<JobResult[]>([]);
  const [refreshSignal, setRefreshSignal] = useState(0);
  const [cancelTarget, setCancelTarget] = useState<string | null>(null);
  const [closeDialogOpen, setCloseDialogOpen] = useState(false);

  const pushResult = useCallback((result: JobResult) => {
    setResults((current) => [...current, result]);
  }, []);

  // 发起任务即乐观登记一条清单项：只有刻录与追加发进度事件，复制、抢救、校验、
  // 制作镜像只能靠这一步出现在“当前任务”清单里（占用标注与停止入口都依赖它）。
  // 每次调用取一个新的负序号，撤销按序号精确移除，不会误删同设备上已在跑的条目。
  const startJob = useCallback((kind: JobKind, device: string) => {
    const seq = nextLocalSeq();
    setJobs((current) =>
      current.some((job) => job.device === device)
        ? current
        : [...current, { kind, device, seq, fraction: null }],
    );
    return seq;
  }, []);

  // 启动被拒（BusySlot、输入被拒）时撤掉刚登记的那条。
  const abortJob = useCallback((seq: number) => {
    setJobs((current) => current.filter((job) => job.seq !== seq));
  }, []);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;
    // StrictMode 挂载两次：订阅真正建立后才登记清理函数，卸载先行则直接反订阅。
    void Promise.all([
      onJobProgress((progress) => {
        setJobs((current) => {
          const index = current.findIndex((item) => item.device === progress.device);
          if (index === -1) {
            return [
              ...current,
              {
                kind: progress.kind,
                device: progress.device,
                seq: progress.seq,
                fraction: progress.fraction,
              },
            ];
          }
          const next = [...current];
          next[index] = { ...next[index], kind: progress.kind, seq: progress.seq, fraction: progress.fraction };
          return next;
        });
      }),
      onJobDone((done) => {
        // 载荷缺 device 或 seq 的事件无法配对（前后端同批发布，正常不会发生），丢弃。
        if (typeof done.device !== "string" || typeof done.seq !== "number") {
          return;
        }
        setJobs((current) => current.filter((item) => item.device !== done.device));
        setResults((current) => [...current, done]);
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

  // 结果横幅只显示最新一条，多条失败的历史仍留在 results 里供页面级弹窗逐条处理。
  const latestResult = results.length > 0 ? results[results.length - 1] : null;
  // 停止确认的文案按目标任务的类别区分：写盘类停止会留下不完整的盘。
  const cancelEntry = jobs.find((job) => job.device === cancelTarget) ?? null;
  const cancelWriting =
    cancelEntry !== null && (cancelEntry.kind === "burn" || cancelEntry.kind === "append");
  const anyWriting = jobs.some((job) => job.kind === "burn" || job.kind === "append");

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">OptiBurn</div>
        <nav className="nav">
          {/* 任务在跑也允许切页：各页面按设备判断占用，切页只切可见性。 */}
          {NAV_ITEMS.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`nav-item${page === item.id ? " active" : ""}`}
              onClick={() => setPage(item.id)}
            >
              <item.icon size={15} />
              {item.label}
            </button>
          ))}
        </nav>
        {/* 检查更新固定在侧栏底部，不可就地更新的安装形态（Linux deb）组件自返回 null。 */}
        <UpdaterSection locked={jobs.length > 0} />
      </aside>
      <main className="content">
        {jobs.length > 0 && (
          <section className="job-bar">
            {jobs.map((job) => (
              <div className="job-entry" key={job.device}>
                <div className="job-status-row">
                  <span className="job-status">
                    {job.device === "build"
                      ? jobStatusText(job)
                      : `${job.device} · ${jobStatusText(job)}`}
                  </span>
                  {(job.kind === "burn" ||
                    job.kind === "append" ||
                    job.kind === "copy" ||
                    job.kind === "salvage") && (
                    <button
                      type="button"
                      className="btn btn-danger"
                      onClick={() => setCancelTarget(job.device)}
                    >
                      {t("Stop", "停止")}
                    </button>
                  )}
                </div>
                <ProgressBar fraction={job.fraction} />
              </div>
            ))}
          </section>
        )}
        {latestResult !== null && (
          <div
            className={`result-banner${latestResult.outcome === "done" ? "" : " warn"}`}
          >
            {latestResult.message}
          </div>
        )}
        {/* 四个页面全部保持挂载，切换只切可见性：读盘得到的清单、选择状态与正在
            进行的复制都不会因为切页而丢失。 */}
        <div className={page === "devices" ? undefined : "page-hidden"}>
          <DevicesPage
            jobs={jobs}
            results={results}
            pushResult={pushResult}
            startJob={startJob}
            abortJob={abortJob}
            refreshSignal={refreshSignal}
            active={page === "devices"}
          />
        </div>
        <div className={page === "build" ? undefined : "page-hidden"}>
          <BuildPage
            locked={jobs.some((job) => job.kind === "build")}
            jobs={jobs}
            results={results}
            pushResult={pushResult}
            startJob={startJob}
            abortJob={abortJob}
            active={page === "build"}
          />
        </div>
        <div className={page === "burn" ? undefined : "page-hidden"}>
          <BurnPage
            jobs={jobs}
            results={results}
            pushResult={pushResult}
            startJob={startJob}
            abortJob={abortJob}
            active={page === "burn"}
          />
        </div>
        <div className={page === "append" ? undefined : "page-hidden"}>
          <AppendPage
            jobs={jobs}
            results={results}
            pushResult={pushResult}
            startJob={startJob}
            abortJob={abortJob}
            active={page === "append"}
          />
        </div>
      </main>
      <ConfirmDialog
        open={cancelTarget !== null}
        title={t("Stop task", "停止任务")}
        message={
          cancelWriting
            ? t(
                "Stopping now leaves the disc with incomplete contents and an unclosed track. On many drives that makes the disc unwritable, so only reading (and rescuing) it will be possible afterwards. Stop anyway?",
                "停止后盘片内容不完整，留下未关闭的轨道。多数驱动器会因此拒绝再写这张盘，之后只能读盘（以及从里面抢救数据）。确认停止？",
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
          const device = cancelTarget;
          setCancelTarget(null);
          // 任务可能恰好刚结束：此时取消请求会被后端拒绝，而 job-done 已更新界面，忽略即可。
          if (device !== null) {
            cancelJob(device).catch(() => undefined);
          }
        }}
        onCancel={() => setCancelTarget(null)}
      />
      <ConfirmDialog
        open={closeDialogOpen}
        title={t("Quit OptiBurn", "退出 OptiBurn")}
        message={
          anyWriting
            ? t(
                "A task is running. Stopping now leaves the disc with incomplete contents and an unclosed track, which on many drives makes the disc unwritable. Stop and quit anyway?",
                "有任务正在进行。停止后盘片内容不完整，留下未关闭的轨道，多数驱动器会因此拒绝再写这张盘。确认停止并退出？",
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
