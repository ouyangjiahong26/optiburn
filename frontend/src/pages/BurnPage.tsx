// 刻录：命令立即返回，进度与结果全部来自事件；门禁失败用对话框引导。
import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { RefreshCw } from "lucide-react";
import { speedOption, startBurn, startVerify } from "../api";
import { useDeviceProbe, useGateNotice } from "../hooks";
import { discStatusLabel } from "../discStatus";
import { t } from "../i18n";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { FormRow } from "../components/FormRow";
import { PathField } from "../components/PathField";
import { VerifyCard } from "../components/VerifyCard";
import type { DiscPageProps, VerifyReport } from "../types";

export function BurnPage({ locked, result, active, onJobStart, onJobAbort }: DiscPageProps) {
  const [image, setImage] = useState("");
  const [device, setDevice] = useState("");
  const [speed, setSpeed] = useState("");
  const [closeDisc, setCloseDisc] = useState(false);
  const [verify, setVerify] = useState<VerifyReport | null>(null);
  const { devices, probing, refresh } = useDeviceProbe();
  const { notice: gateNotice, dismiss: dismissGate } = useGateNotice("burn", result);

  // 页面常驻挂载，探测只在页面可见时刷新：切回来时按当前盘片重读状态。
  useEffect(() => {
    if (active) {
      void refresh();
    }
  }, [active, refresh]);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setVerify(null);
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

  async function handleVerify() {
    setVerify(null);
    onJobStart("verify");
    try {
      setVerify(await startVerify({ device, source: image.trim(), files: [], mode: "burn" }));
    } catch (cause) {
      onJobAbort("verify", String(cause));
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>{t("Burn", "刻录")}</h1>
          <p>{t("Write an image file to a blank disc.", "把镜像文件写入一张空盘。")}</p>
        </div>
      </header>
      <form className="form" onSubmit={(event) => void handleSubmit(event)}>
        <FormRow label={t("Image file", "镜像文件")}>
          <PathField
            mode="openIso"
            value={image}
            onChange={setImage}
            disabled={locked}
            placeholder={t("Choose an .iso image", "选择 .iso 镜像")}
          />
        </FormRow>
        <FormRow label={t("Device", "设备")}>
          <div className="inline-controls">
            <select
              value={device}
              disabled={locked || probing}
              onChange={(event) => setDevice(event.target.value)}
            >
              <option value="">
                {devices === null
                  ? t("Probing devices…", "正在探测设备……")
                  : devices.length === 0
                    ? t("No optical drives found", "未发现光驱")
                    : t("Select a device", "请选择设备")}
              </option>
              {devices !== null &&
                devices.map((item) => (
                  <option key={item.path} value={item.path}>
                    {t(
                      `${item.path} (${item.status === null ? "Unknown" : discStatusLabel(item.status)})`,
                      `${item.path}（${item.status === null ? "状态未知" : discStatusLabel(item.status)}）`,
                    )}
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
              {t("Refresh", "刷新")}
            </button>
          </div>
        </FormRow>
        <FormRow label={t("Speed", "倍速")} htmlFor="burn-speed">
          <input
            id="burn-speed"
            type="number"
            className="text-input speed-input"
            min={1}
            placeholder={t("Leave blank to let the drive choose", "留空交给驱动自选")}
            value={speed}
            disabled={locked}
            onChange={(event) => setSpeed(event.target.value)}
          />
        </FormRow>
        <FormRow
          label={t("Finalize disc", "写完封盘")}
          hint={t("Cannot append after finalizing", "勾选后不能再追加")}
        >
          <label className="option">
            <input
              type="checkbox"
              checked={closeDisc}
              disabled={locked}
              onChange={(event) => setCloseDisc(event.target.checked)}
            />
            {t("Finalize after burning", "刻录完成后封盘")}
          </label>
        </FormRow>
        <div className="form-actions">
          <button
            type="button"
            className="btn"
            disabled={locked || image.trim() === "" || device === ""}
            onClick={() => void handleVerify()}
          >
            {t("Verify disc", "校验盘片")}
          </button>
          <button
            type="submit"
            className="btn btn-primary"
            disabled={locked || image.trim() === "" || device === ""}
          >
            {t("Burn", "开始刻录")}
          </button>
        </div>
      </form>
      {verify !== null && (
        <VerifyCard
          report={verify}
          passNote={t("Disc contents match the image item by item.", "盘上内容与镜像逐项一致。")}
        />
      )}
      <ConfirmDialog
        open={gateNotice !== null}
        title={t("Cannot burn this disc", "无法刻录这张盘")}
        message={gateNotice ?? ""}
        confirmText={t("OK", "知道了")}
        onConfirm={dismissGate}
        onCancel={dismissGate}
      />
    </div>
  );
}
