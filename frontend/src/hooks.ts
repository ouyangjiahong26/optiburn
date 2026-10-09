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

// 门禁类失败按契约靠文案识别（含“追加”“封口”或“挂载”），用对话框引导换盘、
// 改用追加页或先卸载光盘。seq 保证每条结果只提示一次，关掉后不会因重新渲染再弹。
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
      result.seq <= handledSeq.current
    ) {
      return;
    }
    if (!/追加|封口|挂载/.test(result.message)) {
      return;
    }
    handledSeq.current = result.seq;
    setNotice(result.message);
  }, [kind, result]);

  const dismiss = useCallback(() => setNotice(null), []);
  return { notice, dismiss };
}
