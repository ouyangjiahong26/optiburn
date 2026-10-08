// 四种盘片状态的中文文案，设备徽标与刻录、追加页的设备下拉共用。
import type { DiscStatus } from "./types";

export const DISC_STATUS_LABEL: Record<DiscStatus, string> = {
  empty: "空盘",
  appendable: "可追加",
  finalized: "已封口",
  other: "随机可写",
};
