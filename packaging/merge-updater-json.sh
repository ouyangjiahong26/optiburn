#!/usr/bin/env bash
# 由各平台 updater 签名产物（裸 .AppImage / *-setup.exe 及其 .sig）合成 updater
# 元数据 latest.json。tauri build 只产出签名产物，不生成 latest.json，本脚本在
# release 流程中承担这一职责（ADR-0015）。
# 输入参数：
#   $1 - 搜索目录（如 dist）
#   $2 - 输出文件路径（如 dist/latest.json）
#   $3 - 版本号（如 0.1.4，无 v 前缀）
#   $4 - 发布说明文件路径（可选，作为 notes 原样放进 latest.json）
# 环境变量：GITHUB_REPOSITORY（形如 owner/repo），用于拼产物下载 URL。

set -euo pipefail

SEARCH_DIR="${1:?缺少搜索目录}"
OUTPUT_FILE="${2:?缺少输出文件路径}"
VERSION="${3:?缺少版本号}"
NOTES_FILE="${4:-}"
REPO="${GITHUB_REPOSITORY:?缺少 GITHUB_REPOSITORY 环境变量（形如 owner/repo）}"

node -e '
const fs = require("fs");
const path = require("path");

const [searchDir, outputFile, version, notesFile, repo] = process.argv.slice(1);
const tag = `v${version.replace(/^v/, "")}`;

let notes;
if (notesFile && fs.existsSync(notesFile)) {
  notes = fs.readFileSync(notesFile, "utf8");
}

function walk(dir) {
  const results = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) results.push(...walk(full));
    else results.push(full);
  }
  return results;
}

// 产物文件名后缀 → updater 平台键（{os}-{arch}，见 tauri-plugin-updater）。
// Linux 的 updater 产物就是裸 .AppImage 本体（v2 原生），Windows 同理是 setup.exe。
const RULES = [
  [/_amd64\.AppImage$/, "linux-x86_64"],
  [/_aarch64\.AppImage$/, "linux-aarch64"],
  [/_x64-setup\.exe$/, "windows-x86_64"],
  [/_arm64-setup\.exe$/, "windows-aarch64"],
];

const files = walk(searchDir);
const platforms = {};
for (const [pattern, platform] of RULES) {
  const bundle = files.find((f) => pattern.test(f));
  if (!bundle) {
    console.error(`错误：未找到 ${platform} 的 updater 产物（匹配 ${pattern}）`);
    process.exit(1);
  }
  const sigFile = `${bundle}.sig`;
  if (!fs.existsSync(sigFile)) {
    console.error(`错误：缺少签名文件 ${sigFile}`);
    process.exit(1);
  }
  platforms[platform] = {
    signature: fs.readFileSync(sigFile, "utf8").trim(),
    url: `https://github.com/${repo}/releases/download/${tag}/${path.basename(bundle)}`,
  };
}

const manifest = {
  version: tag,
  notes: notes || undefined,
  pub_date: new Date().toISOString(),
  platforms,
};
fs.writeFileSync(outputFile, JSON.stringify(manifest, null, 2), "utf8");
console.log(`已生成 ${outputFile}，包含平台: ${Object.keys(platforms).join(", ")}`);
' "${SEARCH_DIR}" "${OUTPUT_FILE}" "${VERSION}" "${NOTES_FILE}" "${REPO}"
