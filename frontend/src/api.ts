// 命令与事件的唯一出入口：页面不直接接触 @tauri-apps 的模块。
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  DeviceInfo,
  DiscProfile,
  ImageInfoDto,
  JobDone,
  JobProgress,
} from "./types";

export function probeDevices(): Promise<DeviceInfo[]> {
  return invoke("probe_devices");
}

export function startBuildImage(args: {
  src: string;
  output: string | null;
  profile: DiscProfile;
  volumeId: string;
}): Promise<ImageInfoDto> {
  return invoke("start_build_image", args);
}

export function startBurn(args: {
  image: string;
  device: string;
  speed: number | null;
  closeDisc: boolean;
}): Promise<void> {
  return invoke("start_burn", args);
}

export function startAppend(args: {
  src: string;
  device: string;
  volumeId: string;
  speed: number | null;
  closeDisc: boolean;
}): Promise<void> {
  return invoke("start_append", args);
}

export function cancelJob(): Promise<void> {
  return invoke("cancel_job");
}

export function confirmClose(): Promise<void> {
  return invoke("confirm_close");
}

// 三个订阅都返回清理函数（Promise 形式），StrictMode 下的 effect 卸载靠它收尾。
export function onJobProgress(
  handler: (progress: JobProgress) => void,
): Promise<UnlistenFn> {
  return listen<JobProgress>("job-progress", (event) => handler(event.payload));
}

export function onJobDone(handler: (done: JobDone) => void): Promise<UnlistenFn> {
  return listen<JobDone>("job-done", (event) => handler(event.payload));
}

export function onCloseBlocked(handler: () => void): Promise<UnlistenFn> {
  return listen("close-blocked", () => handler());
}

// 倍速留空表示交给驱动自选；非正数同样按留空处理，避免把垃圾值发给后端。
export function speedOption(value: string): number | null {
  const parsed = Number(value);
  return value.trim() === "" || !Number.isFinite(parsed) || parsed <= 0
    ? null
    : parsed;
}
