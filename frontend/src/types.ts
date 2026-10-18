// 与 src-tauri 命令层的 DTO 逐字段对应（camelCase）。字段改名等于破坏 IPC 契约。

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

// 回读校验结果：差异与读不出的文件都为空才算通过，条目是按界面语言生成的描述，直接展示。
export type VerifyReport = {
  differences: string[];
  unreadable: string[];
};

export type DiscProfile = "cd" | "dvd" | "bd";

export type JobKind = "build" | "burn" | "append" | "verify" | "copy" | "salvage";

// 门禁类失败的标记：后端随 job-done 一起发，前端据此弹引导对话框而不是靠文案匹配。
export type GateKind =
  | "append"
  | "finalized"
  | "mounted"
  | "noIsoSession"
  | "capacity"
  | "damagedSession";

export type JobOutcome = "done" | "cancelled" | "failed";

// 进度事件载荷：device 是任务槽键（设备串，build 为 "build"），seq 与完成事件配对。
export type JobProgress = { kind: JobKind; device: string; seq: number; fraction: number };

// 完成事件载荷：device 与 seq 的口径同 JobProgress。
export type JobDone = {
  kind: JobKind;
  device: string;
  seq: number;
  outcome: JobOutcome;
  message: string;
  gate?: GateKind;
};

// 前端自己发起但被立即拒绝的调用不会再来 job-done 事件，就地合成一条结果：
// seq 用负数自减（见 hooks.ts 的 nextLocalSeq），与后端从 1 起的序号永不冲突。
export type JobResult = JobDone;

// 运行中的任务：fraction 为 null 表示没有进度回调（不确定形态）。
export type ActiveJob = { kind: JobKind; device: string; seq: number; fraction: number | null };

// 四个页面统一的 props：App 持有全部任务状态，页面按 device 判断各自关心的占用。
export type PageProps = {
  jobs: ActiveJob[];
  results: JobResult[];
  pushResult: (result: JobResult) => void;
  // 发起任务即乐观登记一条清单项（本地负序号），返回的序号供启动被拒时精确撤销。
  // 只有刻录与追加有进度回调，其余任务的清单项只能靠这一步出现。
  startJob: (kind: JobKind, device: string) => number;
  abortJob: (seq: number) => void;
  // 页面是否可见：设备探测只在可见时刷新（页面常驻挂载，不等于一直活跃）。
  active: boolean;
};
