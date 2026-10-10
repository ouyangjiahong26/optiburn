// 面向人的容量格式化：按量级取 GB/MB/KB/字节，数值与单位之间留一个空格。
// 设备页与追加页共用，与后端 human_bytes 同口径。
import { t } from "./i18n";

export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }
  if (bytes >= 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${bytes} ${t("bytes", "字节")}`;
}
