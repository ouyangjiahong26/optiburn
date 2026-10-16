// 侧栏左下角的版本提示：只展示本机应用版本（Tauri 的 getVersion，源头是
// src-tauri/tauri.conf.json 的 version）。它与“检查更新”入口无关：Linux 的 deb
// 安装与开发态运行不能就地更新，那里不出现更新按钮，版本号照样要在（ADR-0015
// 约束的是更新通道，不是版本提示）。
import { useEffect, useState } from "react";
import { appVersion } from "../api";

export function VersionBadge() {
  const [version, setVersion] = useState("");
  useEffect(() => {
    // 读不到版本时留品牌名，吞掉 rejection：版本提示不是关键路径，不该往控制台
    // 抛未处理的失败。
    void appVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  return (
    <div className="version-badge">
      {version === "" ? "OptiBurn" : `OptiBurn v${version}`}
    </div>
  );
}
