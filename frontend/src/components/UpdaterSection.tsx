// 侧栏底部的“检查更新”：只有可就地更新的安装形态显示（Windows NSIS、Linux AppImage，
// 见 ADR-0015）。检查、确认、下载、重启都在这里闭环；写盘任务进行中入口禁用，
// 下载结束时若任务仍在跑则不自动重启，把完成安装留给用户下一次启动。
import { useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { t } from "../i18n";
import {
  canSelfUpdate,
  checkForUpdate,
  installUpdate,
  relaunchApp,
  type Update,
} from "../api";
import { ConfirmDialog } from "./ConfirmDialog";

type Phase = "idle" | "checking" | "confirm" | "downloading";

export function UpdaterSection({ locked }: { locked: boolean }) {
  const [enabled, setEnabled] = useState(false);
  const [version, setVersion] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [update, setUpdate] = useState<Update | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  // 下载完成那一刻要读最新的任务状态，回调闭包里拿不到当次渲染的 prop。
  const lockedRef = useRef(locked);
  useEffect(() => {
    lockedRef.current = locked;
  }, [locked]);

  useEffect(() => {
    void canSelfUpdate().then(setEnabled);
    void getVersion().then(setVersion);
  }, []);

  if (!enabled) {
    return null;
  }

  async function startCheck() {
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
        t(
          `Check for updates failed: ${error}`,
          `检查更新失败：${error}`,
        ),
      );
      setPhase("idle");
    }
  }

  async function startInstall() {
    if (update === null) {
      setPhase("idle");
      return;
    }
    setPhase("downloading");
    setStatus(t("Downloading update…", "正在下载更新……"));
    try {
      await installUpdate(update, (fraction) => {
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
        setStatus(
          t(
            "Update downloaded. Restart the app after the current task finishes.",
            "更新已下载，当前任务结束后重启应用即可用上新版本。",
          ),
        );
        setPhase("idle");
        return;
      }
      await relaunchApp();
    } catch (error) {
      setStatus(t(`Update failed: ${error}`, `更新失败：${error}`));
      setPhase("idle");
    }
  }

  const busy = phase !== "idle";
  return (
    <div className="updater">
      <div className="updater-version">
        {version === "" ? "OptiBurn" : `OptiBurn v${version}`}
      </div>
      <button
        type="button"
        className="btn updater-button"
        disabled={locked || busy}
        onClick={() => void startCheck()}
      >
        {phase === "checking"
          ? t("Checking…", "正在检查……")
          : phase === "downloading"
            ? t("Updating…", "正在更新……")
            : t("Check for updates", "检查更新")}
      </button>
      {status !== null && <div className="updater-status">{status}</div>}
      <ConfirmDialog
        open={phase === "confirm"}
        title={t("Update available", "发现新版本")}
        message={
          update === null
            ? ""
            : t(
                `The current version is v${version}; v${update.version} is available. The app restarts when the update finishes.`,
                `当前版本 v${version}，可更新到 v${update.version}。更新完成后应用会自动重启。`,
              )
        }
        confirmText={t("Download and install", "下载并安装")}
        cancelText={t("Not now", "暂不更新")}
        onConfirm={() => void startInstall()}
        onCancel={() => {
          setUpdate(null);
          setPhase("idle");
        }}
      />
    </div>
  );
}
