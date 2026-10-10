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

// 容量文案：总容量与可用容量各自可缺，都没有时返回 null（整段省略）。
// 设备页卡片与追加页设备行共用，两处的口径与措辞才不会漂移。
export function capacityLabel(total: number | null, free: number | null): string | null {
  if (total !== null && free !== null) {
    return t(
      `${formatBytes(total)} total, ${formatBytes(free)} free`,
      `总容量 ${formatBytes(total)}，可用 ${formatBytes(free)}`,
    );
  }
  if (total !== null) {
    return t(`${formatBytes(total)} total`, `总容量 ${formatBytes(total)}`);
  }
  if (free !== null) {
    return t(`${formatBytes(free)} free`, `可用 ${formatBytes(free)}`);
  }
  return null;
}
