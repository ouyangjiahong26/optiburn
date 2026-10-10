// 与 src-tauri 命令层的 DTO 逐字段对应（camelCase）；字段改名等于破坏 IPC 契约。

export type DiscStatus = "empty" | "appendable" | "finalized" | "other";

export type DeviceInfo = {
  path: string;
  identity: string | null;
  status: DiscStatus | null;
  statusBits: number | null;
  sessions: number | null;
  // 盘片容量（字节）。两个口径各自可缺，读不到时为 null，界面按能读到的部分显示。
  capacityBytes: number | null;
  freeBytes: number | null;
  error: string | null;
};

// 盘上条目：path 以 / 开头，目录 size 为 0。
export type DiscEntry = {
  path: string;
  size: number;
  isDir: boolean;
};

export type ImageInfoDto = {
  sectors: number;
  bytes: number;
  filesystems: string[];
};

// 回读校验结果：差异为空表示盘上内容与源一致，条目是按界面语言生成的描述，直接展示。
export type VerifyReport = {
  differences: string[];
};

export type DiscProfile = "cd" | "dvd" | "bd";

export type JobKind = "build" | "burn" | "append" | "verify" | "copy";

// 门禁类失败的标记：后端随 job-done 一起发，前端据此弹引导对话框而不是靠文案匹配。
export type GateKind = "append" | "finalized" | "mounted" | "noIsoSession" | "capacity";

export type JobOutcome = "done" | "cancelled" | "failed";

export type JobProgress = { kind: JobKind; fraction: number };

export type JobDone = {
  kind: JobKind;
  outcome: JobOutcome;
  message: string;
  gate?: GateKind;
};

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
  // 页面是否可见：设备探测只在可见时刷新（页面常驻挂载，不等于一直活跃）。
  active: boolean;
};
