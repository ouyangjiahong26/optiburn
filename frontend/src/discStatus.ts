// 四种盘片状态的双语文案，设备徽标与刻录、追加页的设备下拉共用。
import type { DiscStatus } from "./types";
import { t } from "./i18n";

export const DISC_STATUS_LABEL: Record<DiscStatus, { en: string; zh: string }> = {
  empty: { en: "Empty", zh: "空盘" },
  appendable: { en: "Appendable", zh: "可追加" },
  finalized: { en: "Finalized", zh: "已封口" },
  other: { en: "Rewritable", zh: "随机可写" },
};

// 按当前语言取盘片状态文案。
export function discStatusLabel(status: DiscStatus): string {
  return t(DISC_STATUS_LABEL[status].en, DISC_STATUS_LABEL[status].zh);
}
