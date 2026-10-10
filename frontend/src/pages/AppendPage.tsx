// 追加：把待刻录文件写入空盘或已有区段的盘，文件在盘根平铺。
import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import { RefreshCw, X } from "lucide-react";
import { discVolumeId, pasteFiles, pickFiles, speedOption, startAppend, startVerify } from "../api";
import { useDeviceProbe, useGateNotice } from "../hooks";
import { discStatusLabel } from "../discStatus";
import { capacityLabel } from "../format";
import { t } from "../i18n";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { FormRow } from "../components/FormRow";
import { VerifyCard } from "../components/VerifyCard";
import type { DiscPageProps, VerifyReport } from "../types";

export function AppendPage({ locked, result, active, onJobStart, onJobAbort }: DiscPageProps) {
  const [files, setFiles] = useState<string[]>([]);
  const [device, setDevice] = useState("");
  const [volumeId, setVolumeId] = useState("OPTIBURN");
  const [speed, setSpeed] = useState("");
  const [closeDisc, setCloseDisc] = useState(false);
  const [verify, setVerify] = useState<VerifyReport | null>(null);
  const [pasteNote, setPasteNote] = useState<string | null>(null);
  const { devices, probing, refresh } = useDeviceProbe();
  const { notice: gateNotice, dismiss: dismissGate } = useGateNotice("append", result);

  // 页面常驻挂载，探测只在页面可见时刷新：切回来时按当前盘片重读状态。
  useEffect(() => {
    if (active) {
      void refresh();
    }
  }, [active, refresh]);

  // 选中设备后读盘上现有卷标预填，避免以默认值静默改掉盘上的卷标。读不到（空盘、
  // 盘被挂载占用等）就回到默认卷标，不把上一张盘的值带进新盘。序号守卫防迟到的
  // 响应覆盖后选的设备，手改过的值不再被自动写回覆盖。
  const volumeSeqRef = useRef(0);
  const volumeDirtyRef = useRef(false);
  const refreshVolumeId = useCallback(async () => {
    if (device === "") {
      return;
    }
    const seq = ++volumeSeqRef.current;
    try {
      const id = await discVolumeId(device);
      if (volumeSeqRef.current === seq && !volumeDirtyRef.current) {
        setVolumeId(id);
      }
    } catch {
      if (volumeSeqRef.current === seq && !volumeDirtyRef.current) {
        setVolumeId("OPTIBURN");
      }
    }
  }, [device]);

  // 页面可见时重读卷标：切页返回也能感知换盘（device 没变也要重读）。
  useEffect(() => {
    if (active) {
      void refreshVolumeId();
    }
  }, [active, refreshVolumeId]);

  // 添加入口共用：去重后并入列表。同名不同路径的文件由后端在暂存阶段拒绝。
  const addFiles = useCallback((paths: string[]) => {
    setFiles((current) => {
      const merged = [...current];
      for (const path of paths) {
        if (!merged.includes(path)) {
          merged.push(path);
        }
      }
      return merged;
    });
  }, []);

  // Ctrl+V 把系统剪贴板里的文件加进列表。WebKit 的 paste 事件在焦点不在可编辑
  // 控件上时不可靠（实测无响应），这里改为拦截按键再经 GTK 读剪贴板。焦点在输入
  // 框里时不拦截，让文本正常粘贴。
  const handlePasteFromClipboard = useCallback(async () => {
    setPasteNote(null);
    try {
      const paths = await pasteFiles();
      if (paths.length === 0) {
        setPasteNote(
          t(
            "No files in the clipboard. Copy files in the file manager first, then come back to this page.",
            "剪贴板里没有文件。请先在文件管理器里复制文件，再回到本页。",
          ),
        );
        return;
      }
      addFiles(paths);
    } catch (cause) {
      setPasteNote(String(cause));
    }
  }, [addFiles]);

  // 监听只在追加页可见且没有任务时生效：页面常驻挂载，全局监听会让其它页面按
  // Ctrl+V 也往这里加文件（ADR-0011 的粘贴语义是页面内）。
  useEffect(() => {
    if (!active || locked) {
      return;
    }
    function handleKeyDown(event: KeyboardEvent) {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "v") {
        return;
      }
      const target = event.target as HTMLElement | null;
      const editable =
        target !== null &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable);
      if (editable) {
        return;
      }
      event.preventDefault();
      void handlePasteFromClipboard();
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [active, locked, handlePasteFromClipboard]);

  const ready = files.length > 0 && device !== "";

  async function handlePickFiles() {
    addFiles(await pickFiles());
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setVerify(null);
    onJobStart("append");
    try {
      await startAppend({
        files,
        device,
        volumeId: volumeId.trim(),
        speed: speedOption(speed),
        closeDisc,
      });
    } catch (cause) {
      onJobAbort("append", String(cause));
    }
  }

  async function handleVerify() {
    setVerify(null);
    onJobStart("verify");
    try {
      setVerify(await startVerify({ device, source: "", files, mode: "append" }));
    } catch (cause) {
      onJobAbort("verify", String(cause));
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>{t("Append", "追加")}</h1>
          <p>{t("Write files to a blank disc or one with existing sessions; on a blank disc this is the first write.", "把待刻录文件写入空盘或已有区段的盘，空盘上就是首刻。")}</p>
        </div>
      </header>
      <form className="form" onSubmit={(event) => void handleSubmit(event)}>
        <FormRow label={t("Files to burn", "待刻录文件")}>
          <div className="file-picker">
            {files.length === 0 ? (
              <p className="file-empty">
                {t(
                  `No files yet. Click "Add files" to pick several from the file manager, or copy files in the file manager first and press Ctrl+V here to paste. You can also click "Paste".`,
                  "还没有文件。点“添加文件”从文件管理器多选，或先在文件管理器复制文件，回到本页按 Ctrl+V 粘贴，也可以点“粘贴”。",
                )}
              </p>
            ) : (
              <ul className="file-list">
                {files.map((path) => (
                  <li key={path}>
                    <span className="file-name" title={path}>
                      {path.split(/[\\/]/).pop() ?? path}
                    </span>
                    <button
                      type="button"
                      className="btn btn-sm"
                      disabled={locked}
                      aria-label={t(`Remove ${path}`, `移除 ${path}`)}
                      onClick={() =>
                        setFiles((current) => current.filter((item) => item !== path))
                      }
                    >
                      <X size={11} />
                    </button>
                  </li>
                ))}
              </ul>
            )}
            <div className="inline-controls">
              <button
                type="button"
                className="btn"
                disabled={locked}
                onClick={() => void handlePickFiles()}
              >
                {t("Add files", "添加文件")}
              </button>
              <button
                type="button"
                className="btn"
                disabled={locked}
                onClick={() => void handlePasteFromClipboard()}
              >
                {t("Paste", "粘贴")}
              </button>
              <button
                type="button"
                className="btn"
                disabled={locked || files.length === 0}
                onClick={() => setFiles([])}
              >
                {t("Clear", "清空")}
              </button>
            </div>
            {pasteNote !== null && <p className="file-empty">{pasteNote}</p>}
          </div>
        </FormRow>
        <FormRow label={t("Device", "设备")}>
          <div className="inline-controls">
            <select
              value={device}
              disabled={locked || probing}
              onChange={(event) => {
                // 换设备视为换盘：清掉手改标记，让新盘的卷标能重新预填。
                volumeDirtyRef.current = false;
                setDevice(event.target.value);
              }}
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
              onClick={() => {
                void refresh();
                void refreshVolumeId();
              }}
            >
              <RefreshCw size={13} className={probing ? "spin" : undefined} />
              {t("Refresh", "刷新")}
            </button>
          </div>
          {devices !== null &&
            (() => {
              // 选中盘的容量提示：读得到口径才显示。数字来自盘片查询而不是挂载
              // 视图，多区段盘不会被首区段的挂载容量误导。
              const selected = devices.find((item) => item.path === device);
              const label =
                selected === undefined
                  ? null
                  : capacityLabel(selected.capacityBytes, selected.freeBytes);
              if (label === null) {
                return null;
              }
              return (
                <p className="page-note">
                  {t(`On the selected disc: ${label}.`, `所选盘片${label}。`)}
                </p>
              );
            })()}
        </FormRow>
        <FormRow label={t("Volume label", "卷标")} htmlFor="append-volume">
          <input
            id="append-volume"
            type="text"
            className="text-input"
            value={volumeId}
            disabled={locked}
            onChange={(event) => {
              // 手改之后不再自动写回，避免切页返回时覆盖用户输入。
              volumeDirtyRef.current = true;
              setVolumeId(event.target.value);
            }}
          />
        </FormRow>
        <FormRow label={t("Speed", "倍速")} htmlFor="append-speed">
          <input
            id="append-speed"
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
            {t("Finalize after appending", "追加完成后封盘")}
          </label>
        </FormRow>
        <div className="form-actions">
          <button
            type="button"
            className="btn"
            disabled={locked || !ready}
            onClick={() => void handleVerify()}
          >
            {t("Verify disc", "校验盘片")}
          </button>
          <button type="submit" className="btn btn-primary" disabled={locked || !ready}>
            {t("Append", "开始追加")}
          </button>
        </div>
      </form>
      {verify !== null && (
        <VerifyCard
          report={verify}
          passNote={t("Disc contents match the selected files item by item.", "盘上内容与所选文件逐项一致。")}
        />
      )}
      <ConfirmDialog
        open={gateNotice !== null}
        title={t("Cannot append to this disc", "无法追加这张盘")}
        message={gateNotice ?? ""}
        confirmText={t("OK", "知道了")}
        onConfirm={dismissGate}
        onCancel={dismissGate}
      />
    </div>
  );
}
