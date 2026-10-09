// 命令与事件的唯一出入口：页面不直接接触 @tauri-apps 的模块。
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  DeviceInfo,
  DiscEntry,
  DiscProfile,
  ImageInfoDto,
  JobDone,
  JobProgress,
  VerifyReport,
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
  files: string[];
  device: string;
  volumeId: string;
  speed: number | null;
  closeDisc: boolean;
}): Promise<void> {
  return invoke("start_append", args);
}

// 弹出系统文件选择框（多选文件），取消返回空数组。
export async function pickFiles(): Promise<string[]> {
  const picked = await open({ multiple: true, directory: false });
  if (picked === null) {
    return [];
  }
  return Array.isArray(picked) ? picked : [picked];
}

// 读系统剪贴板里的文件列表（文件管理器复制后的粘贴入口）。
export function pasteFiles(): Promise<string[]> {
  return invoke("paste_files");
}

// 读盘上现有卷标，供追加页预填。读不到时调用方保持默认值。
export function discVolumeId(device: string): Promise<string> {
  return invoke("disc_volume_id", { device });
}

// 列出盘上最后一区段的内容（只读）。
export function listDisc(device: string): Promise<DiscEntry[]> {
  return invoke("list_disc", { device });
}

// 把盘上选中的文件复制到系统剪贴板，文件管理器里粘贴即可；完成结果走 job-done 事件。
export function copyDiscFiles(device: string, paths: string[]): Promise<void> {
  return invoke("copy_disc_files", { device, paths });
}

// 回读校验：把盘上内容与源（追加的待刻录文件或刻录的镜像）逐文件对比。
export function startVerify(args: {
  device: string;
  source: string;
  files: string[];
  mode: "burn" | "append";
}): Promise<VerifyReport> {
  return invoke("start_verify", args);
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

// 倍速留空表示交给驱动自选；小数与超出 u32 的值同样按留空处理：
// 后端反序列化不了，会以英文报错拒绝并透传给界面。
export function speedOption(value: string): number | null {
  const parsed = Number(value);
  return value.trim() === "" ||
    !Number.isFinite(parsed) ||
    parsed <= 0 ||
    !Number.isInteger(parsed) ||
    parsed > 0xffffffff
    ? null
    : parsed;
}
