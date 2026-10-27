// 前端唯一的持久化设置：多设备写入开关（ADR-0025 补记）。默认关闭，刻录页与
// 追加页按它决定单设备表单还是设备配对表。存 localStorage：这是界面形态选择，
// 不影响写盘行为，不值得为它加一条 IPC。
const MULTI_DEVICE_KEY = "optiburn:multi-device";

export function loadMultiDevice(): boolean {
  try {
    return localStorage.getItem(MULTI_DEVICE_KEY) === "1";
  } catch {
    // WebView 禁用存储时按默认（单设备）处理，开关退化为会话内状态。
    return false;
  }
}

export function saveMultiDevice(enabled: boolean): void {
  try {
    localStorage.setItem(MULTI_DEVICE_KEY, enabled ? "1" : "0");
  } catch {
    // 读不到就存不进，界面状态照常生效，仅不跨会话保留。
  }
}
