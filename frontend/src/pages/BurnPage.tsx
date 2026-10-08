// 刻录：命令立即返回，进度与结果全部来自事件；门禁失败用对话框引导。
import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { RefreshCw } from "lucide-react";
import { speedOption, startBurn } from "../api";
import { useDeviceProbe, useGateNotice } from "../hooks";
import { DISC_STATUS_LABEL } from "../discStatus";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { FormRow } from "../components/FormRow";
import { PathField } from "../components/PathField";
import type { DiscPageProps } from "../types";

export function BurnPage({ locked, result, onJobStart, onJobAbort }: DiscPageProps) {
  const [image, setImage] = useState("");
  const [device, setDevice] = useState("");
  const [speed, setSpeed] = useState("");
  const [closeDisc, setCloseDisc] = useState(false);
  const { devices, probing, refresh } = useDeviceProbe();
  const { notice: gateNotice, dismiss: dismissGate } = useGateNotice("burn", result);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    onJobStart("burn");
    try {
      await startBurn({
        image: image.trim(),
        device,
        speed: speedOption(speed),
        closeDisc,
      });
    } catch (cause) {
      onJobAbort("burn", String(cause));
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>刻录</h1>
          <p>把镜像文件写入一张空盘。</p>
        </div>
      </header>
      <form className="form" onSubmit={(event) => void handleSubmit(event)}>
        <FormRow label="镜像文件">
          <PathField
            mode="openIso"
            value={image}
            onChange={setImage}
            disabled={locked}
            placeholder="选择 .iso 镜像"
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
        <FormRow label="倍速" htmlFor="burn-speed">
          <input
            id="burn-speed"
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
            刻录完成后封盘
          </label>
        </FormRow>
        <div className="form-actions">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={locked || image.trim() === "" || device === ""}
          >
            开始刻录
          </button>
        </div>
      </form>
      <ConfirmDialog
        open={gateNotice !== null}
        title="无法刻录这张盘"
        message={gateNotice ?? ""}
        confirmText="知道了"
        onConfirm={dismissGate}
        onCancel={dismissGate}
      />
    </div>
  );
}
