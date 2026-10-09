// 四种盘片状态各配一色：empty 绿、appendable 黄、finalized 红、other 强调色。
import type { DiscStatus } from "../types";
import { discStatusLabel } from "../discStatus";

export function StatusBadge({ status }: { status: DiscStatus }) {
  return (
    <span className={`badge badge-${status}`}>{discStatusLabel(status)}</span>
  );
}
