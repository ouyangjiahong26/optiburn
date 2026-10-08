// 追加：在可追加的盘上继续写入目录，交互与刻录页一致。
import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { RefreshCw } from "lucide-react";
import { speedOption, startAppend } from "../api";
import { useDeviceProbe, useGateNotice } from "../hooks";
import { DISC_STATUS_LABEL } from "../discStatus";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { FormRow } from "../components/FormRow";
import { PathField } from "../components/PathField";
import type { DiscPageProps } from "../types";

export function AppendPage({ locked, result, onJobStart, onJobAbort }: DiscPageProps) {
  const [src, setSrc] = useState("");
  const [device, setDevice] = useState("");
  const [volumeId, setVolumeId] = useState("OPTIBURN");
  const [speed, setSpeed] = useState("");
  const [closeDisc, setCloseDisc] = useState(false);
  const { devices, probing, refresh } = useDeviceProbe();
  const { notice: gateNotice, dismiss: dismissGate } = useGateNotice("append", result);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    onJobStart("append");
    try {
      await startAppend({
        src: src.trim(),
        device,
        volumeId: volumeId.trim(),
        speed: speedOption(speed),
        closeDisc,
      });
    } catch (cause) {
      onJobAbort("append", String(cause));
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>追加</h1>
          <p>在已有数据区段的盘上继续写入一个目录。</p>
        </div>
      </header>
      <form className="form" onSubmit={(event) => void handleSubmit(event)}>
        <FormRow label="源目录">
          <PathField
            mode="directory"
            value={src}
            onChange={setSrc}
            disabled={locked}
            placeholder="选择要追加的目录"
          />
        </FormRow>
        <FormRow label="设备">
          <div className="inline-controls">
            <select
              value={device}
              disabled={locked || probing}
              onChange={(event) => setDevice(event.target.value)}
            >
              <option value="">
                {devices === null
                  ? "正在探测设备……"
                  : devices.length === 0
                    ? "未发现光驱"
                    : "请选择设备"}
              </option>
              {devices !== null &&
                devices.map((item) => (
                  <option key={item.path} value={item.path}>
                    {`${item.path}（${item.status === null ? "状态未知" : DISC_STATUS_LABEL[item.status]}）`}
                  </option>
                ))}
            </select>
            <button
              type="button"
              className="btn"
              disabled={locked || probing}
              onClick={() => void refresh()}
            >
              <RefreshCw size={13} className={probing ? "spin" : undefined} />
              刷新
            </button>
          </div>
        </FormRow>
        <FormRow label="卷标" htmlFor="append-volume">
          <input
            id="append-volume"
            type="text"
            className="text-input"
            value={volumeId}
            disabled={locked}
            onChange={(event) => setVolumeId(event.target.value)}
          />
        </FormRow>
        <FormRow label="倍速" htmlFor="append-speed">
          <input
            id="append-speed"
            type="number"
            className="text-input speed-input"
            min={1}
            placeholder="留空交给驱动自选"
            value={speed}
            disabled={locked}
            onChange={(event) => setSpeed(event.target.value)}
          />
        </FormRow>
        <FormRow label="写完封盘" hint="勾选后不能再追加">
          <label className="option">
            <input
              type="checkbox"
              checked={closeDisc}
              disabled={locked}
              onChange={(event) => setCloseDisc(event.target.checked)}
            />
            追加完成后封盘
          </label>
        </FormRow>
        <div className="form-actions">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={locked || src.trim() === "" || device === ""}
          >
            开始追加
          </button>
        </div>
      </form>
      <ConfirmDialog
        open={gateNotice !== null}
        title="无法追加这张盘"
        message={gateNotice ?? ""}
        confirmText="知道了"
        onConfirm={dismissGate}
        onCancel={dismissGate}
      />
    </div>
  );
}
