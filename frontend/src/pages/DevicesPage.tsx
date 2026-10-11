// 默认落地页：探测在页面可见时刷新，job-done 后由 App 发信号重读。
// 点击设备卡片展开盘上文件清单，支持拖动框选与 Ctrl/Shift 多选，可复制到系统剪贴板。
import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { Copy, FileText, Folder, LifeBuoy, RefreshCw } from "lucide-react";
import { copyDiscFiles, listDisc, pickSalvageFolder, salvageDisc } from "../api";
import { useDeviceProbe } from "../hooks";
import { t } from "../i18n";
import { capacityLabel, formatBytes } from "../format";
import type { DiscEntry, JobControls } from "../types";
import { StatusBadge } from "../components/StatusBadge";

type ListingState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; entries: DiscEntry[] };

export function DevicesPage({
  locked,
  refreshSignal,
  active,
  onJobStart,
  onJobAbort,
}: JobControls & {
  locked: boolean;
  refreshSignal: number;
  active: boolean;
}) {
  const { devices, probing, error, refresh } = useDeviceProbe();
  const [openDevice, setOpenDevice] = useState<string | null>(null);
  const [listing, setListing] = useState<ListingState | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const anchorRef = useRef<string | null>(null);
  const draggingRef = useRef(false);
  // 清单加载的请求序号：迟到的响应按序号丢弃（见 loadListing）。
  const listingSeqRef = useRef(0);

  // 页面常驻挂载，探测只在页面可见时刷新：切回来时按当前盘片重读状态。重探时
  // 展开中的清单一并重读，盘可能已经换过。
  useEffect(() => {
    if (active) {
      void refresh();
      reloadExpanded();
    }
  }, [active, refresh]);

  useEffect(() => {
    // 隐藏时不做重探：页面可见时本就会重探重读，避免在别的页面跑探针与 xorriso。
    if (refreshSignal === 0 || !active) {
      return;
    }
    void refresh();
    // 任务结束后盘上内容可能已变：展开中的清单重读，选择与锚点清零。
    reloadExpanded();
  }, [refreshSignal, refresh]);

  // 拖动框选在指针抬起时收尾，指针可能落在列表外，所以挂在 document 上。
  useEffect(() => {
    function stopDrag() {
      draggingRef.current = false;
    }
    document.addEventListener("pointerup", stopDrag);
    return () => document.removeEventListener("pointerup", stopDrag);
  }, []);

  const files = listing?.kind === "ready" ? listing.entries.filter((entry) => !entry.isDir) : [];
  const selectedBytes = files
    .filter((entry) => selected.includes(entry.path))
    .reduce((sum, entry) => sum + entry.size, 0);

  async function loadListing(path: string) {
    // 序号守卫：切换设备或收起清单后，迟到的响应不许覆盖当前状态。
    const seq = ++listingSeqRef.current;
    setListing({ kind: "loading" });
    try {
      const entries = await listDisc(path);
      if (listingSeqRef.current === seq) {
        setListing({ kind: "ready", entries });
      }
    } catch (cause) {
      if (listingSeqRef.current === seq) {
        setListing({ kind: "error", message: String(cause) });
      }
    }
  }

  // 重读展开中的清单：设备重探（切页回来或点刷新）后盘可能已换，旧清单与选择作废。
  function reloadExpanded() {
    if (openDevice === null) {
      return;
    }
    anchorRef.current = null;
    setSelected([]);
    void loadListing(openDevice);
  }

  async function toggleDevice(path: string) {
    if (locked) {
      return;
    }
    // 锚点与进行中的加载都属于上一份清单，切换时一并作废。
    anchorRef.current = null;
    listingSeqRef.current += 1;
    if (openDevice === path) {
      setOpenDevice(null);
      setListing(null);
      setSelected([]);
      return;
    }
    setOpenDevice(path);
    setSelected([]);
    void loadListing(path);
  }

  function selectRange(fromPath: string, toPath: string) {
    const fromIndex = files.findIndex((entry) => entry.path === fromPath);
    const toIndex = files.findIndex((entry) => entry.path === toPath);
    if (fromIndex < 0 || toIndex < 0) {
      return;
    }
    const [low, high] = fromIndex <= toIndex ? [fromIndex, toIndex] : [toIndex, fromIndex];
    setSelected(files.slice(low, high + 1).map((entry) => entry.path));
  }

  function handleRowDown(event: ReactPointerEvent, entry: DiscEntry) {
    if (entry.isDir || locked) {
      return;
    }
    if (event.shiftKey && anchorRef.current !== null) {
      selectRange(anchorRef.current, entry.path);
      return;
    }
    if (event.ctrlKey || event.metaKey) {
      anchorRef.current = entry.path;
      setSelected((current) =>
        current.includes(entry.path)
          ? current.filter((path) => path !== entry.path)
          : [...current, entry.path],
      );
      return;
    }
    anchorRef.current = entry.path;
    draggingRef.current = true;
    setSelected([entry.path]);
  }

  function handleRowEnter(entry: DiscEntry) {
    if (!draggingRef.current || entry.isDir || anchorRef.current === null) {
      return;
    }
    selectRange(anchorRef.current, entry.path);
  }

  async function handleCopy() {
    if (openDevice === null || selected.length === 0 || locked) {
      return;
    }
    // 复制在任务槽里跑，进度与完成消息走全局任务事件，与写盘同一套展示。
    onJobStart("copy");
    try {
      await copyDiscFiles(openDevice, selected);
    } catch (cause) {
      onJobAbort("copy", String(cause));
    }
  }

  // 抢救未关闭轨道：先选目标目录，再占任务槽跑到结束。结果（完整、半截、没写
  // 各多少，半截 zip 怎么处置）随完成消息给出。
  async function handleSalvage() {
    if (openDevice === null || locked) {
      return;
    }
    const dest = await pickSalvageFolder();
    if (dest === null) {
      return;
    }
    onJobStart("salvage");
    try {
      await salvageDisc(openDevice, dest);
    } catch (cause) {
      onJobAbort("salvage", String(cause));
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>{t("Devices", "设备")}</h1>
          <p>{t("View local optical drives and disc status. Click a disc to browse its files.", "查看本机光驱与盘片状态。点击盘片可以查看盘上文件。")}</p>
        </div>
        <button
          type="button"
          className="btn"
          disabled={locked || probing}
          onClick={() => {
            void refresh();
            reloadExpanded();
          }}
        >
          <RefreshCw size={13} className={probing ? "spin" : undefined} />
          {t("Refresh", "刷新")}
        </button>
      </header>
      {error !== null && (
        <div className="result-banner warn">
          {t(`Device probe failed: ${error}`, `探测设备失败：${error}`)}
        </div>
      )}
      {devices === null ? (
        error === null && (
          <p className="page-note">{t("Probing devices…", "正在探测设备……")}</p>
        )
      ) : devices.length === 0 ? (
        <div className="empty-state">{t("No optical drives found", "未发现光驱")}</div>
      ) : (
        <ul className="device-list">
          {devices.map((device) => {
            const capacity = capacityLabel(device.capacityBytes, device.freeBytes);
            return (
            <li
              key={device.path}
              className={`device-card${openDevice === device.path ? " expanded" : ""}`}
            >
              <div
                className="device-main device-main-clickable"
                role="button"
                tabIndex={0}
                aria-expanded={openDevice === device.path}
                onClick={() => void toggleDevice(device.path)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    void toggleDevice(device.path);
                  }
                }}
              >
                <span className="device-path">{device.path}</span>
                {device.status !== null && <StatusBadge status={device.status} />}
                <span className="device-toggle">
                  {openDevice === device.path
                    ? t("Collapse", "收起")
                    : t("Browse files", "查看盘上文件")}
                </span>
              </div>
              <div className="device-meta">
                <span>{device.identity ?? t("Unknown model", "未知型号")}</span>
                {device.sessions !== null && (
                  <span>
                    {t(
                      device.sessions === 1
                        ? "1 session"
                        : `${device.sessions} sessions`,
                      `${device.sessions} 个区段`,
                    )}
                  </span>
                )}
                {capacity !== null && <span>{capacity}</span>}
              </div>
              {device.error !== null && (
                <p className="device-error">{device.error}</p>
              )}
              {openDevice === device.path && (
                <div className="disc-listing">
                  {device.status === "appendable" && (
                    <div className="disc-toolbar">
                      <button
                        type="button"
                        className="btn"
                        disabled={locked}
                        onClick={() => void handleSalvage()}
                      >
                        <LifeBuoy size={11} />
                        {t("Salvage interrupted write", "抢救中断写入的数据")}
                      </button>
                      <span className="disc-hint">
                        {t(
                          "If a burn was interrupted, the data written up to the cut is still on the disc but not in the disc's visible directory. This reads it out into a folder you pick; the report says which files are complete and which are cut short.",
                          "如果上一次刻录被中断，写到中断点为止的数据还在盘上，但不在盘的可见目录里。这个动作把它读到您选的目录；结果会说明哪些文件完整、哪些只写了一半。",
                        )}
                      </span>
                    </div>
                  )}
                  {listing?.kind === "loading" && (
                    <p className="page-note">
                      {t("Reading disc contents…", "正在读取盘上内容……")}
                    </p>
                  )}
                  {listing?.kind === "error" && (
                    <p className="device-error">{listing.message}</p>
                  )}
                  {listing?.kind === "ready" && (
                    <>
                      <div className="disc-toolbar">
                        <span className="disc-count">
                          {t(
                            files.length === 1
                              ? "1 file"
                              : `${files.length} files`,
                            `共 ${files.length} 个文件`,
                          )}
                          {t(
                            selected.length === 1
                              ? ", 1 selected"
                              : `, ${selected.length} selected`,
                            `，已选 ${selected.length} 个`,
                          )}
                          {selected.length > 0
                            ? t(
                                ` (${formatBytes(selectedBytes)})`,
                                `（${formatBytes(selectedBytes)}）`,
                              )
                            : ""}
                        </span>
                        <button
                          type="button"
                          className="btn"
                          disabled={locked || selected.length === 0}
                          onClick={() => void handleCopy()}
                        >
                          <Copy size={11} />
                          {t("Copy selected files", "复制选中文件")}
                        </button>
                      </div>
                      {listing.entries.length === 0 ? (
                        <p className="file-empty">
                          {t("No files on the disc.", "盘上没有文件。")}
                        </p>
                      ) : (
                        <ul className="disc-entries">
                          {listing.entries.map((entry) => {
                            const name = entry.path.split("/").pop() ?? entry.path;
                            const depth = entry.path.split("/").length - 2;
                            return (
                              <li
                                key={entry.path}
                                className={`disc-entry${entry.isDir ? " dir" : ""}${
                                  selected.includes(entry.path) ? " selected" : ""
                                }`}
                                style={{ paddingLeft: `${8 + depth * 14}px` }}
                                title={entry.path}
                                onPointerDown={(event) => handleRowDown(event, entry)}
                                onPointerEnter={() => handleRowEnter(entry)}
                              >
                                {entry.isDir ? <Folder size={12} /> : <FileText size={12} />}
                                <span className="disc-entry-name">{name}</span>
                                {!entry.isDir && (
                                  <span className="disc-entry-size">
                                    {formatBytes(entry.size)}
                                  </span>
                                )}
                              </li>
                            );
                          })}
                        </ul>
                      )}
                      <p className="disc-hint">
                        {t(
                          "Click to select, hold and drag to select a range, Ctrl or Shift to add. Copying first reads the files off the disc (this takes a moment); once it finishes, paste them in your system file manager. Keep this app open until you have pasted.",
                          "单击选中，按住指针拖动可框选，Ctrl 或 Shift 加选。复制会先把文件从光盘读出来（需要一点时间），完成后到系统文件管理器里粘贴即可。粘贴前请不要关闭本应用。",
                        )}
                      </p>
                    </>
                  )}
                </div>
              )}
            </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
