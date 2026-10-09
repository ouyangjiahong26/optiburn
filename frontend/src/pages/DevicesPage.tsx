// 默认落地页：挂载时探测一次，job-done 后留在本页时由 App 发信号自动重探。
// 点击设备卡片展开盘上文件清单，支持拖动框选与 Ctrl/Shift 多选，可复制到系统剪贴板。
import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { Copy, FileText, Folder, RefreshCw } from "lucide-react";
import { copyDiscFiles, listDisc } from "../api";
import { useDeviceProbe } from "../hooks";
import type { DiscEntry } from "../types";
import { StatusBadge } from "../components/StatusBadge";

type ListingState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; entries: DiscEntry[] };

// 按量级格式化大小，数值与单位之间留一个空格。
function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }
  if (bytes >= 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${bytes} 字节`;
}

export function DevicesPage({
  locked,
  refreshSignal,
}: {
  locked: boolean;
  refreshSignal: number;
}) {
  const { devices, probing, error, refresh } = useDeviceProbe();
  const [openDevice, setOpenDevice] = useState<string | null>(null);
  const [listing, setListing] = useState<ListingState | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [copying, setCopying] = useState(false);
  const [copyResult, setCopyResult] = useState<{ ok: boolean; message: string } | null>(null);
  const anchorRef = useRef<string | null>(null);
  const draggingRef = useRef(false);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (refreshSignal === 0) {
      return;
    }
    void refresh();
    // 任务结束后盘上内容可能已变：展开中的清单重读，选择与复制结果清零。
    if (openDevice !== null) {
      setSelected([]);
      setCopyResult(null);
      void loadListing(openDevice);
    }
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
    setListing({ kind: "loading" });
    try {
      setListing({ kind: "ready", entries: await listDisc(path) });
    } catch (cause) {
      setListing({ kind: "error", message: String(cause) });
    }
  }

  async function toggleDevice(path: string) {
    if (locked) {
      return;
    }
    if (openDevice === path) {
      setOpenDevice(null);
      setListing(null);
      setSelected([]);
      setCopyResult(null);
      return;
    }
    setOpenDevice(path);
    setSelected([]);
    setCopyResult(null);
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
    if (entry.isDir || copying) {
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
    if (openDevice === null || selected.length === 0 || copying) {
      return;
    }
    setCopying(true);
    setCopyResult(null);
    try {
      const report = await copyDiscFiles(openDevice, selected);
      setCopyResult({
        ok: true,
        message: `已复制 ${report.count} 个文件（${formatBytes(report.bytes)}）到系统剪贴板，去文件管理器里粘贴即可。`,
      });
    } catch (cause) {
      setCopyResult({ ok: false, message: String(cause) });
    } finally {
      setCopying(false);
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>设备</h1>
          <p>查看本机光驱与盘片状态。点击盘片可以查看盘上文件。</p>
        </div>
        <button
          type="button"
          className="btn"
          disabled={locked || probing}
          onClick={() => void refresh()}
        >
          <RefreshCw size={13} className={probing ? "spin" : undefined} />
          刷新
        </button>
      </header>
      {error !== null && (
        <div className="result-banner warn">探测设备失败：{error}</div>
      )}
      {devices === null ? (
        error === null && <p className="page-note">正在探测设备……</p>
      ) : devices.length === 0 ? (
        <div className="empty-state">未发现光驱</div>
      ) : (
        <ul className="device-list">
          {devices.map((device) => (
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
                  {openDevice === device.path ? "收起" : "查看盘上文件"}
                </span>
              </div>
              <div className="device-meta">
                <span>{device.identity ?? "未知型号"}</span>
                {device.sessions !== null && (
                  <span>{device.sessions} 个区段</span>
                )}
              </div>
              {device.error !== null && (
                <p className="device-error">{device.error}</p>
              )}
              {openDevice === device.path && (
                <div className="disc-listing">
                  {listing?.kind === "loading" && (
                    <p className="page-note">正在读取盘上内容……</p>
                  )}
                  {listing?.kind === "error" && (
                    <p className="device-error">{listing.message}</p>
                  )}
                  {listing?.kind === "ready" && (
                    <>
                      <div className="disc-toolbar">
                        <span className="disc-count">
                          共 {files.length} 个文件，已选 {selected.length} 个
                          {selected.length > 0 ? `（${formatBytes(selectedBytes)}）` : ""}
                        </span>
                        <button
                          type="button"
                          className="btn"
                          disabled={locked || copying || selected.length === 0}
                          onClick={() => void handleCopy()}
                        >
                          <Copy size={11} />
                          {copying ? "复制中……" : "复制选中文件"}
                        </button>
                      </div>
                      {listing.entries.length === 0 ? (
                        <p className="file-empty">盘上没有文件。</p>
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
                        单击选中，按住指针拖动可框选，Ctrl 或 Shift 加选。复制会先把文件从光盘读出来（需要一点时间），完成后到系统文件管理器里粘贴即可。粘贴前请不要关闭本应用。
                      </p>
                      {copyResult !== null && (
                        <p className={copyResult.ok ? "disc-note" : "device-error"}>
                          {copyResult.message}
                        </p>
                      )}
                    </>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
