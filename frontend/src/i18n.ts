// 界面语言：跟随系统语言。zh 开头（zh-CN、zh-TW 等）用中文，其余一律英文——
// 默认国际化，未覆盖语言回退到英文（AppImageHub 与 WinGet 的国际用户依赖这一点）。
export type Lang = "en" | "zh";

export const LANG: Lang = (navigator.language ?? "")
  .toLowerCase()
  .startsWith("zh")
  ? "zh"
  : "en";

// 让浏览器与读屏器知道界面语言（影响字体回退与无障碍朗读）。
document.documentElement.lang = LANG === "zh" ? "zh-CN" : "en";

// 界面文案就地成对书写：t("英文", "中文")。带插值的句子在两个参数里各自写成
// 模板字符串，保证语序可以随语言调整。
export function t(en: string, zh: string): string {
  return LANG === "zh" ? zh : en;
}
