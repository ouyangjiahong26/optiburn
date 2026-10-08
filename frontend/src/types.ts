// 与 src-tauri 命令层的 DTO 逐字段对应（camelCase）；字段改名等于破坏 IPC 契约。

export type DiscStatus = "empty" | "appendable" | "finalized" | "other";

export type DeviceInfo = {
  path: string;
  identity: string | null;
  status: DiscStatus | null;
  statusBits: number | null;
  sessions: number | null;
  error: string | null;
};

export type ImageInfoDto = {
  sectors: number;
  bytes: number;
  filesystems: string[];
};

export type DiscProfile = "cd" | "dvd" | "bd";

export type JobKind = "build" | "burn" | "append";

export type JobOutcome = "done" | "cancelled" | "failed";

export type JobProgress = { kind: JobKind; fraction: number };

export type JobDone = { kind: JobKind; outcome: JobOutcome; message: string };

// 前端自己发起但被立即拒绝的调用不会再来 job-done 事件，就地生成一条结果；
// seq 用于区分先后，保证对话框之类的消费方对每条结果只反应一次。
export type JobResult = JobDone & { seq: number };

// 运行中的任务：fraction 为 null 表示没有进度回调（不确定形态）。
export type ActiveJob = { kind: JobKind; fraction: number | null };

// 页面启动与解锁任务的统一入口，由 App 提供，保证全局只有一处任务状态。
export type JobControls = {
  onJobStart: (kind: JobKind) => void;
  onJobAbort: (kind: JobKind, message?: string) => void;
};

// 刻录页与追加页共有的 props。
export type DiscPageProps = JobControls & {
  locked: boolean;
  result: JobResult | null;
};
