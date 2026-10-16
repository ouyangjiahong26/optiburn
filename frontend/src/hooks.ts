// 页面级复用逻辑：设备探测（三个页面）、门禁失败提示（刻录、追加两页）与本地
// 被拒结果的序号生成。
import { useCallback, useEffect, useRef, useState } from "react";
import { probeDevices } from "./api";
import type { DeviceInfo, JobKind, JobResult } from "./types";

export function useDeviceProbe() {
  const [devices, setDevices] = useState<DeviceInfo[] | null>(null);
  const [probing, setProbing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setProbing(true);
    setError(null);
    try {
      setDevices(await probeDevices());
    } catch (cause) {
      setError(String(cause));
    } finally {
      setProbing(false);
    }
  }, []);

  return { devices, probing, error, refresh };
}

// 门禁类失败由后端在 job-done 里标记 gate 种类（追加改道、封口换盘、挂载先卸载、
// UDF 盘不支持续写），用对话框引导。多设备并发后同类失败可能一次来多条：只展示
// 第一条未处理过的，关掉一条才轮到下一条，逐台引导（issue #50）。seq 已入集的
// 结果不再展示，StrictMode 或重复渲染都不会再弹。
export function useGateNotice(
  kind: JobKind,
  results: JobResult[],
): { notice: string | null; dismiss: () => void } {
  const [shown, setShown] = useState<{ seq: number; message: string } | null>(null);
  const handledSeq = useRef(new Set<number>());

  useEffect(() => {
    if (shown !== null) {
      return;
    }
    const pending = results.find(
      (item) =>
        item.kind === kind &&
        item.outcome === "failed" &&
        item.gate !== undefined &&
        !handledSeq.current.has(item.seq),
    );
    if (pending !== undefined) {
      // 展示即入集：一条结果只弹一次，dismiss 只负责清出当前这条。
      handledSeq.current.add(pending.seq);
      setShown({ seq: pending.seq, message: pending.message });
    }
  }, [kind, results, shown]);

  const dismiss = useCallback(() => setShown(null), []);
  return { notice: shown?.message ?? null, dismiss };
}

// 本地被拒（BusySlot、空列表）没有后端事件，前端就地合成结果。序号用负数自减：
// 后端序号从 1 起，两套永不冲突。全前端共用一个计数器，负数之间也不撞号。
let localSeq = 0;
export function nextLocalSeq(): number {
  localSeq -= 1;
  return localSeq;
}
