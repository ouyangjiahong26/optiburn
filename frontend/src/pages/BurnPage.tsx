// 刻录：设备与镜像配对成行，多行提交即多设备并发（issue #50）。命令立即返回，
// 进度与结果全部来自事件，门禁失败用对话框逐条引导。
import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { Plus, RefreshCw, X } from "lucide-react";
import { speedOption, startBurn, startVerify } from "../api";
import { nextLocalSeq, useDeviceProbe, useGateNotice } from "../hooks";
import { discStatusLabel } from "../discStatus";
import { t } from "../i18n";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { FormRow } from "../components/FormRow";
import { PathField } from "../components/PathField";
import { VerifyCard } from "../components/VerifyCard";
import type { PageProps, VerifyReport } from "../types";

type BurnRow = {
  id: number;
  device: string;
  image: string;
  error: string | null;
  verify: VerifyReport | null;
};

// 行 id 用模块级递增计数器：行会增删，key 必须跨重渲染稳定。
let nextRowId = 1;

export function BurnPage({ jobs, results, pushResult, startJob, abortJob, active }: PageProps) {
  const [rows, setRows] = useState<BurnRow[]>([]);
  const [speed, setSpeed] = useState("");
  const [closeDisc, setCloseDisc] = useState(false);
  const { devices, probing, refresh } = useDeviceProbe();
  const { notice: gateNotice, dismiss: dismissGate } = useGateNotice("burn", results);

  // 页面常驻挂载，探测只在页面可见时刷新：切回来时按当前盘片重读状态。
  useEffect(() => {
    if (active) {
      void refresh();
    }
  }, [active, refresh]);

  const busyDevices = new Set(jobs.map((job) => job.device));

  function updateRow(id: number, patch: Partial<BurnRow>) {
    setRows((current) =>
      current.map((row) => (row.id === id ? { ...row, ...patch } : row)),
    );
  }

  // 无可用设备（全部被其它行选走或全部占用）时不能加行。
  const takenDevices = new Set(
    rows.map((row) => row.device).filter((device) => device !== ""),
  );
  const canAddRow =
    devices !== null && devices.some((item) => !takenDevices.has(item.path));

  const readyRows = rows.filter(
    (row) => row.device !== "" && row.image.trim() !== "",
  );

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    for (const row of readyRows) {
      try {
        // 逐行顺序提交（issue 的逐行契约）：被拒的行错误落在行内，不影响已发起的行。
        await startBurn({
          image: row.image.trim(),
          device: row.device,
          speed: speedOption(speed),
          closeDisc,
        });
        updateRow(row.id, { error: null });
      } catch (cause) {
        updateRow(row.id, { error: String(cause) });
      }
    }
  }

  async function handleVerify(row: BurnRow) {
    updateRow(row.id, { verify: null });
    // 校验没有进度回调：先乐观登记清单项，占用标注与启动被拒的撤销都靠它。
    const seq = startJob("verify", row.device);
    try {
      const report = await startVerify({
        device: row.device,
        source: row.image.trim(),
        files: [],
        mode: "burn",
      });
      updateRow(row.id, { verify: report });
    } catch (cause) {
      abortJob(seq);
      pushResult({
        kind: "verify",
        device: row.device,
        seq: nextLocalSeq(),
        outcome: "failed",
        message: String(cause),
      });
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>{t("Burn", "刻录")}</h1>
          <p>{t("Write an image file to a blank disc.", "把镜像文件写入一张空盘。")}</p>
          <p className="page-note">
            {t("Burning to several drives at once: see ", "一次向多台光驱写入：见")}
            <a
              href="https://github.com/ouyangjiahong26/optiburn/blob/main/docs/MULTI-DRIVE.md"
              target="_blank"
              rel="noreferrer"
            >
              {t("Multi-drive writing", "多设备写入说明")}
            </a>
            {t(".", "。")}
          </p>
        </div>
      </header>
      <form className="form" onSubmit={(event) => void handleSubmit(event)}>
        {rows.map((row) => {
          const rowBusy = busyDevices.has(row.device);
          const rowReady = row.device !== "" && row.image.trim() !== "";
          return (
            <fieldset className="pair-row" key={row.id}>
              <FormRow label={t("Device", "设备")} htmlFor={`burn-device-${row.id}`}>
                <div className="inline-controls">
                  <select
                    disabled={rowBusy || probing}
                    value={row.device}
                    onChange={(event) =>
                      updateRow(row.id, { device: event.target.value, error: null })
                    }
                  >
                    <option value="">
                      {devices === null
                        ? t("Probing devices…", "正在探测设备……")
                        : devices.length === 0
                          ? t("No optical drives found", "未发现光驱")
                          : t("Select a device", "请选择设备")}
                    </option>
                    {devices !== null &&
                      devices.map((item) => {
                        // 其它行已选的设备不进选项，被任务占用的进选项但禁用。
                        if (row.device !== item.path && takenDevices.has(item.path)) {
                          return null;
                        }
                        const busy = busyDevices.has(item.path);
                        return (
                          <option key={item.path} value={item.path} disabled={busy}>
                            {t(
                              `${item.path} (${item.status === null ? "Unknown" : discStatusLabel(item.status)})`,
                              `${item.path}（${item.status === null ? "状态未知" : discStatusLabel(item.status)}）`,
                            )}
                            {busy ? t(" (busy)", "（任务进行中）") : ""}
                          </option>
                        );
                      })}
                  </select>
                  <button
                    type="button"
                    className="btn"
                    disabled={probing}
                    onClick={() => void refresh()}
                  >
                    <RefreshCw size={13} className={probing ? "spin" : undefined} />
                    {t("Refresh", "刷新")}
                  </button>
                  <button
                    type="button"
                    className="btn"
                    onClick={() =>
                      setRows((current) => current.filter((item) => item.id !== row.id))
                    }
                  >
                    <X size={13} />
                    {t("Remove", "移除")}
                  </button>
                </div>
              </FormRow>
              <FormRow label={t("Image file", "镜像文件")}>
                <PathField
                  mode="openIso"
                  value={row.image}
                  onChange={(image) => updateRow(row.id, { image, error: null })}
                  disabled={rowBusy}
                  placeholder={t("Choose an .iso image", "选择 .iso 镜像")}
                />
              </FormRow>
              {row.error !== null && <p className="device-error">{row.error}</p>}
              <div className="inline-controls">
                <button
                  type="button"
                  className="btn"
                  // 设备被占用时不允许校验：校验与写盘争同一台光驱（issue 用户故事）。
                  disabled={!rowReady || rowBusy}
                  onClick={() => void handleVerify(row)}
                >
                  {t("Verify disc", "校验盘片")}
                </button>
              </div>
              {row.verify !== null && (
                <VerifyCard
                  report={row.verify}
                  passNote={t(
                    "Disc contents match the image item by item.",
                    "盘上内容与镜像逐项一致。",
                  )}
                />
              )}
            </fieldset>
          );
        })}
        <div className="inline-controls">
          <button
            type="button"
            className="btn"
            disabled={!canAddRow}
            onClick={() =>
              setRows((current) => [
                ...current,
                { id: nextRowId++, device: "", image: "", error: null, verify: null },
              ])
            }
          >
            <Plus size={13} />
            {t("Add device", "添加设备")}
          </button>
        </div>
        <FormRow label={t("Speed", "倍速")} htmlFor="burn-speed">
          <input
            id="burn-speed"
            type="number"
            className="text-input speed-input"
            min={1}
            placeholder={t("Leave blank to let the drive choose", "留空交给驱动自选")}
            value={speed}
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
              onChange={(event) => setCloseDisc(event.target.checked)}
            />
            {t("Finalize after burning", "刻录完成后封盘")}
          </label>
        </FormRow>
        <div className="form-actions">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={readyRows.length === 0}
          >
            {t("Burn", "开始刻录")}
          </button>
        </div>
      </form>
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
