// 页面级复用逻辑：设备探测（三个页面）与门禁失败提示（刻录、追加两页）。
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
// UDF 盘不支持续写），用对话框引导。seq 保证每条结果只提示一次，关掉后不会因
// 重新渲染再弹。
export function useGateNotice(
  kind: JobKind,
  result: JobResult | null,
): { notice: string | null; dismiss: () => void } {
  const [notice, setNotice] = useState<string | null>(null);
  const handledSeq = useRef(0);

  useEffect(() => {
    if (
      result === null ||
      result.kind !== kind ||
      result.outcome !== "failed" ||
      result.gate === undefined ||
      result.seq <= handledSeq.current
    ) {
      return;
    }
    handledSeq.current = result.seq;
    setNotice(result.message);
  }, [kind, result]);

  const dismiss = useCallback(() => setNotice(null), []);
  return { notice, dismiss };
}
