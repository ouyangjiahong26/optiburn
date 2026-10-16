// 追加：把待刻录文件写入空盘或已有区段的盘，文件在盘根平铺。设备与内容配对成
// 行，多行提交即多设备并发（issue #50），门禁失败用对话框逐条引导。
import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import { Plus, RefreshCw, X } from "lucide-react";
import { discVolumeId, pasteFiles, pickFiles, speedOption, startAppend, startVerify } from "../api";
import { nextLocalSeq, useDeviceProbe, useGateNotice } from "../hooks";
import { discStatusLabel } from "../discStatus";
import { capacityLabel } from "../format";
import { t } from "../i18n";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { FormRow } from "../components/FormRow";
import { VerifyCard } from "../components/VerifyCard";
import type { PageProps, VerifyReport } from "../types";

type AppendRow = {
  id: number;
  device: string;
  files: string[];
  volumeId: string;
  volumeDirty: boolean;
  allowDamaged: boolean;
  error: string | null;
  verify: VerifyReport | null;
};

// 行 id 用模块级递增计数器：行会增删，key 必须跨重渲染稳定。
let nextRowId = 1;

const DEFAULT_VOLUME_ID = "OPTIBURN";

export function AppendPage({ jobs, results, pushResult, startJob, abortJob, active }: PageProps) {
  const [rows, setRows] = useState<AppendRow[]>([]);
  const [speed, setSpeed] = useState("");
  const [backend, setBackend] = useState("auto");
  const [closeDisc, setCloseDisc] = useState(false);
  const [pasteNote, setPasteNote] = useState<string | null>(null);
  const { devices, probing, refresh } = useDeviceProbe();
  const { notice: gateNotice, dismiss: dismissGate } = useGateNotice("append", results);

  // 最新行列表的只读视图：键盘监听等回调里要拿当前行，又不随每次编辑重建。
  const rowsRef = useRef(rows);
  useEffect(() => {
    rowsRef.current = rows;
  }, [rows]);
  // 粘贴目标行：最近聚焦（或按住）的行，没有记录时落到第一行。
  const activeRowIdRef = useRef<number | null>(null);

  // 页面常驻挂载，探测只在页面可见时刷新：切回来时按当前盘片重读状态。
  useEffect(() => {
    if (active) {
      void refresh();
    }
  }, [active, refresh]);

  // 选中设备后读盘上现有卷标预填，避免以默认值静默改掉盘上的卷标。读不到（空盘、
  // 盘被挂载占用等）就回到默认卷标，不把别的盘的值带过来。序号守卫按行区分，防
  // 迟到的响应覆盖后选的设备，手改过的行不再被自动写回覆盖。
  const volumeSeqRef = useRef(new Map<number, number>());
  const refreshVolumeId = useCallback(async (rowId: number, device: string) => {
    if (device === "") {
      return;
    }
    const seq = (volumeSeqRef.current.get(rowId) ?? 0) + 1;
    volumeSeqRef.current.set(rowId, seq);
    try {
      const id = await discVolumeId(device);
      if (volumeSeqRef.current.get(rowId) !== seq) {
        return;
      }
      setRows((current) =>
        current.map((row) =>
          row.id === rowId && row.device === device && !row.volumeDirty
            ? { ...row, volumeId: id }
            : row,
        ),
      );
    } catch {
      if (volumeSeqRef.current.get(rowId) !== seq) {
        return;
      }
      setRows((current) =>
        current.map((row) =>
          row.id === rowId && row.device === device && !row.volumeDirty
            ? { ...row, volumeId: DEFAULT_VOLUME_ID }
            : row,
        ),
      );
    }
  }, []);

  // 页面可见时逐行重读卷标：切页返回也能感知换盘（device 没变也要重读）。
  useEffect(() => {
    if (!active) {
      return;
    }
    for (const row of rowsRef.current) {
      void refreshVolumeId(row.id, row.device);
    }
  }, [active, refreshVolumeId]);

  const busyDevices = new Set(jobs.map((job) => job.device));
  const takenDevices = new Set(
    rows.map((row) => row.device).filter((device) => device !== ""),
  );
  const canAddRow =
    devices !== null && devices.some((item) => !takenDevices.has(item.path));

  function updateRow(id: number, patch: Partial<AppendRow>) {
    setRows((current) =>
      current.map((row) => (row.id === id ? { ...row, ...patch } : row)),
    );
  }

  function removeRow(id: number) {
    volumeSeqRef.current.delete(id);
    setRows((current) => current.filter((row) => row.id !== id));
  }

  // 添加入口共用：去重后并入目标行。同名不同路径的文件由后端在暂存阶段拒绝。
  const addFiles = useCallback((rowId: number, paths: string[]) => {
    setRows((current) =>
      current.map((row) => {
        if (row.id !== rowId) {
          return row;
        }
        const merged = [...row.files];
        for (const path of paths) {
          if (!merged.includes(path)) {
            merged.push(path);
          }
        }
        return { ...row, files: merged, error: null };
      }),
    );
  }, []);

  async function handlePickFiles(rowId: number) {
    addFiles(rowId, await pickFiles());
  }

  // Ctrl+V 把系统剪贴板里的文件加进目标行的列表。WebKit 的 paste 事件在焦点不在
  // 可编辑控件上时不可靠（实测无响应），这里改为拦截按键再经 GTK 读剪贴板。焦点
  // 在输入框里时不拦截，让文本正常粘贴。
  const handlePasteFromClipboard = useCallback(async () => {
    setPasteNote(null);
    const target =
      rowsRef.current.find((row) => row.id === activeRowIdRef.current) ??
      rowsRef.current[0];
    // 行列表为空时没有粘贴目标，忽略。
    if (target === undefined) {
      return;
    }
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
      addFiles(target.id, paths);
    } catch (cause) {
      setPasteNote(String(cause));
    }
  }, [addFiles]);

  // 监听只在追加页可见时生效：页面常驻挂载，全局监听会让其它页面按 Ctrl+V 也往
  // 这里加文件（ADR-0011 的粘贴语义是页面内）。
  useEffect(() => {
    if (!active) {
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
  }, [active, handlePasteFromClipboard]);

  const readyRows = rows.filter((row) => row.device !== "" && row.files.length > 0);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    for (const row of readyRows) {
      try {
        // 逐行顺序提交（issue 的逐行契约）：被拒的行错误落在行内，不影响已发起的行。
        await startAppend({
          files: row.files,
          device: row.device,
          volumeId: row.volumeId.trim(),
          options: {
            speed: speedOption(speed),
            closeDisc,
            allowDamagedLastSession: row.allowDamaged,
            backend,
          },
        });
        updateRow(row.id, { error: null });
      } catch (cause) {
        updateRow(row.id, { error: String(cause) });
      }
    }
  }

  async function handleVerify(row: AppendRow) {
    updateRow(row.id, { verify: null });
    // 校验没有进度回调：先乐观登记清单项，占用标注与启动被拒的撤销都靠它。
    const seq = startJob("verify", row.device);
    try {
      const report = await startVerify({
        device: row.device,
        source: "",
        files: row.files,
        mode: "append",
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
          <h1>{t("Append", "追加")}</h1>
          <p>{t("Write files to a blank disc or one with existing sessions; on a blank disc this is the first write.", "把待刻录文件写入空盘或已有区段的盘，空盘上就是首刻。")}</p>
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
          const rowReady = row.device !== "" && row.files.length > 0;
          // 选中盘的容量提示：读得到口径才显示。数字来自盘片查询而不是挂载视图，
          // 多区段盘不会被首区段的挂载容量误导。
          const selected = devices?.find((item) => item.path === row.device);
          const capacity =
            selected === undefined
              ? null
              : capacityLabel(selected.capacityBytes, selected.freeBytes);
          return (
            <fieldset
              className="pair-row"
              key={row.id}
              onFocus={() => {
                activeRowIdRef.current = row.id;
              }}
              onMouseDown={() => {
                activeRowIdRef.current = row.id;
              }}
            >
              <FormRow label={t("Device", "设备")} htmlFor={`append-device-${row.id}`}>
                <div className="inline-controls">
                  <select
                    id={`append-device-${row.id}`}
                    value={row.device}
                    disabled={rowBusy || probing}
                    onChange={(event) => {
                      // 换设备视为换盘：清掉手改标记，让新盘的卷标能重新预填。
                      updateRow(row.id, {
                        device: event.target.value,
                        volumeDirty: false,
                        error: null,
                      });
                      void refreshVolumeId(row.id, event.target.value);
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
                    onClick={() => {
                      void refresh();
                      for (const item of rowsRef.current) {
                        void refreshVolumeId(item.id, item.device);
                      }
                    }}
                  >
                    <RefreshCw size={13} className={probing ? "spin" : undefined} />
                    {t("Refresh", "刷新")}
                  </button>
                  <button
                    type="button"
                    className="btn"
                    onClick={() => removeRow(row.id)}
                  >
                    <X size={13} />
                    {t("Remove", "移除")}
                  </button>
                </div>
                {capacity !== null && (
                  <p className="page-note">
                    {t(`On the selected disc: ${capacity}.`, `所选盘片${capacity}。`)}
                  </p>
                )}
              </FormRow>
              <FormRow label={t("Volume label", "卷标")} htmlFor={`append-volume-${row.id}`}>
                <input
                  id={`append-volume-${row.id}`}
                  type="text"
                  className="text-input"
                  value={row.volumeId}
                  disabled={rowBusy}
                  onChange={(event) => {
                    // 手改之后不再自动写回，避免切页返回时覆盖用户输入。
                    updateRow(row.id, {
                      volumeId: event.target.value,
                      volumeDirty: true,
                    });
                  }}
                />
              </FormRow>
              <FormRow label={t("Files to burn", "待刻录文件")}>
                <div className="file-picker">
                  {row.files.length === 0 ? (
                    <p className="file-empty">
                      {t(
                        `No files yet. Click "Add files" to pick several from the file manager, or copy files in the file manager first and press Ctrl+V here to paste. You can also click "Paste".`,
                        "还没有文件。点“添加文件”从文件管理器多选，或先在文件管理器复制文件，回到本页按 Ctrl+V 粘贴，也可以点“粘贴”。",
                      )}
                    </p>
                  ) : (
                    <ul className="file-list">
                      {row.files.map((path) => (
                        <li key={path}>
                          <span className="file-name" title={path}>
                            {path.split(/[\\/]/).pop() ?? path}
                          </span>
                          <button
                            type="button"
                            className="btn btn-sm"
                            disabled={rowBusy}
                            aria-label={t(`Remove ${path}`, `移除 ${path}`)}
                            onClick={() =>
                              updateRow(row.id, {
                                files: row.files.filter((item) => item !== path),
                              })
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
                      disabled={rowBusy}
                      onClick={() => void handlePickFiles(row.id)}
                    >
                      {t("Add files", "添加文件")}
                    </button>
                    <button
                      type="button"
                      className="btn"
                      disabled={rowBusy}
                      onClick={() => void handlePasteFromClipboard()}
                    >
                      {t("Paste", "粘贴")}
                    </button>
                    <button
                      type="button"
                      className="btn"
                      disabled={rowBusy || row.files.length === 0}
                      onClick={() => updateRow(row.id, { files: [] })}
                    >
                      {t("Clear", "清空")}
                    </button>
                  </div>
                </div>
              </FormRow>
              <FormRow
                label={t("Damaged last session", "损坏的末区段")}
                hint={t(
                  "Only for discs whose last session is unreadable: the append falls back to the previous readable session and files in the skipped sessions disappear from the visible directory",
                  "仅用于末区段读不出来的盘：追加会回退到更早的可读区段，被跳过区段里的文件不再出现在可见目录里",
                )}
              >
                <label className="option">
                  <input
                    type="checkbox"
                    checked={row.allowDamaged}
                    disabled={rowBusy}
                    onChange={(event) =>
                      updateRow(row.id, { allowDamaged: event.target.checked })
                    }
                  />
                  {t("Allow skipping a damaged last session", "允许跳过损坏的末区段")}
                </label>
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
                  passNote={t("Disc contents match the selected files item by item.", "盘上内容与所选文件逐项一致。")}
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
                {
                  id: nextRowId++,
                  device: "",
                  files: [],
                  volumeId: DEFAULT_VOLUME_ID,
                  volumeDirty: false,
                  allowDamaged: false,
                  error: null,
                  verify: null,
                },
              ])
            }
          >
            <Plus size={13} />
            {t("Add device", "添加设备")}
          </button>
        </div>
        {pasteNote !== null && <p className="file-empty">{pasteNote}</p>}
        <FormRow
          label={t("Growth backend", "增长模式后端")}
          hint={t(
            "Follow default uses native growth and falls back to xorriso for random-writable media, unknown media types or a requested speed.",
            "跟随默认用原生增长，随机可写介质、未知介质类型或指定倍速时自动回退 xorriso。",
          )}
        >
          <select
            value={backend}
            disabled={readyRows.length === 0}
            onChange={(event) => setBackend(event.target.value)}
          >
            <option value="auto">{t("Follow default", "跟随默认")}</option>
            <option value="native">{t("Native growth", "原生增长")}</option>
            <option value="xorriso">{t("xorriso grow mode", "xorriso 增长模式")}</option>
          </select>
        </FormRow>
        <FormRow label={t("Speed", "倍速")} htmlFor="append-speed">
          <input
            id="append-speed"
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
            {t("Finalize after appending", "追加完成后封盘")}
          </label>
        </FormRow>
        <div className="form-actions">
          <button
            type="submit"
            className="btn btn-primary"
            disabled={readyRows.length === 0}
          >
            {t("Append", "开始追加")}
          </button>
        </div>
      </form>
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
