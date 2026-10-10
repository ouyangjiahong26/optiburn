// 侧栏底部的“检查更新”：只有可就地更新的安装形态显示（Windows NSIS 与 Linux AppImage，
// 见 ADR-0015）。检查、确认、下载、安装都在这里闭环。下载与安装分两步：Windows 上
// NSIS 安装器接管后会直接退出应用，所以安装只在没有任务进行时触发，进行中的刻录不会
// 被安装动作打断。
import { useEffect, useRef, useState } from "react";
import { t } from "../i18n";
import {
  appVersion,
  applyUpdate,
  canSelfUpdate,
  checkForUpdate,
  discardUpdate,
  downloadUpdate,
  type Update,
} from "../api";
import { ConfirmDialog } from "./ConfirmDialog";

type Phase = "idle" | "checking" | "confirm" | "downloading" | "downloaded";

export function UpdaterSection({ locked }: { locked: boolean }) {
  const [enabled, setEnabled] = useState(false);
  const [version, setVersion] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [update, setUpdate] = useState<Update | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  // 下载与安装是异步过程，回调执行时读不到当次渲染的 prop，用 ref 读最新的任务状态。
  const lockedRef = useRef(locked);
  useEffect(() => {
    lockedRef.current = locked;
  }, [locked]);

  useEffect(() => {
    void canSelfUpdate().then(setEnabled);
    void appVersion().then(setVersion);
  }, []);

  if (!enabled) {
    return null;
  }

  async function startCheck() {
    // 上一轮更新描述若因失败留在手里，重新检查前先释放，避免句柄累积。
    if (update !== null) {
      void discardUpdate(update);
      setUpdate(null);
    }
    setPhase("checking");
    setStatus(null);
    try {
      const found = await checkForUpdate();
      if (found === null) {
        setStatus(t("You are on the latest version.", "已是最新版本。"));
        setPhase("idle");
      } else {
        setUpdate(found);
        setPhase("confirm");
      }
    } catch (error) {
      setStatus(
        t(`Check for updates failed: ${error}`, `检查更新失败：${error}`),
      );
      setPhase("idle");
    }
  }

  async function startDownload() {
    if (update === null) {
      setPhase("idle");
      return;
    }
    setPhase("downloading");
    setStatus(t("Downloading update…", "正在下载更新……"));
    try {
      await downloadUpdate(update, (fraction) => {
        setStatus(
          fraction === null
            ? t("Downloading update…", "正在下载更新……")
            : t(
                `Downloading update… ${Math.round(fraction * 100)}%`,
                `正在下载更新…… ${Math.round(fraction * 100)}%`,
              ),
        );
      });
      if (lockedRef.current) {
        setPhase("downloaded");
        setStatus(
          t(
            "Update downloaded. Install it after the current task finishes.",
            "更新已下载，当前任务结束后即可安装。",
          ),
        );
        return;
      }
      await applyNow();
    } catch (error) {
      setStatus(t(`Update failed: ${error}`, `更新失败：${error}`));
      setPhase("idle");
    }
  }

  // Windows 上安装器接管后应用退出，本函数不会返回。Linux AppImage 是就地替换
  // 文件，装完自动重启进新版本。
  async function applyNow() {
    if (update === null) {
      return;
    }
    setStatus(t("Installing update…", "正在安装更新……"));
    try {
      await applyUpdate(update);
    } catch (error) {
      setStatus(t(`Update failed: ${error}`, `更新失败：${error}`));
      setPhase("idle");
    }
  }

  function cancelUpdate() {
    if (update !== null) {
      void discardUpdate(update);
    }
    setUpdate(null);
    setPhase("idle");
  }

  const busy = phase === "checking" || phase === "downloading";
  return (
    <div className="updater">
      <div className="updater-version">
        {version === "" ? "OptiBurn" : `OptiBurn v${version}`}
      </div>
      {phase === "downloaded" ? (
        <button
          type="button"
          className="btn btn-primary updater-button"
          disabled={locked}
          onClick={() => void applyNow()}
        >
          {t("Install update and restart", "安装更新并重启")}
        </button>
      ) : (
        <button
          type="button"
          className="btn updater-button"
          disabled={locked || busy}
          onClick={() => void startCheck()}
        >
          {phase === "checking"
            ? t("Checking…", "正在检查……")
            : phase === "downloading"
              ? t("Downloading…", "正在下载……")
              : t("Check for updates", "检查更新")}
        </button>
      )}
      {status !== null && <div className="updater-status">{status}</div>}
      <ConfirmDialog
        open={phase === "confirm"}
        title={t("Update available", "发现新版本")}
        message={
          update === null
            ? ""
            : t(
                `The current version is v${version}, and v${update.version} is available. The app restarts when the update finishes.`,
                `当前版本 v${version}，可更新到 v${update.version}。更新完成后应用会自动重启。`,
              )
        }
        confirmText={t("Download", "下载")}
        cancelText={t("Not now", "暂不更新")}
        onConfirm={() => void startDownload()}
        onCancel={cancelUpdate}
      />
    </div>
  );
}
