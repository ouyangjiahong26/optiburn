# ADR-0008：图形前端用 Tauri 2，修订“不加 GUI 依赖”约定

- 状态：已接受
- 日期：2026-10-08

## 背景

目标用户是 Windows 非技术用户，此前 CLI 是唯一入口，四条子命令
（`probe`、`build-image`、`burn`、`append`）对这类用户上手门槛高。

## 决策

新增图形前端：`src-tauri/`（Tauri 2，包 `optiburn-gui`）与 `frontend/`（React +
Vite + TS），两者都在根 cargo workspace 之外，用路径依赖引用四个核心 crate；平台
分支仍然只出现在 `optiburn-transport`。栈与打包形态参考 altgo（Tauri 2、React、
Windows 只发 NSIS 安装包）。GUI 的能力与 CLI 等同：四个子命令各有对应页面，刻录
类任务提供取消按钮。

## 被否决的方案

- egui / iced：纯 Rust 更轻，但用户指定参考 altgo 的 Tauri 栈，且 Tauri 的
  HTML/CSS 让中文渲染与表单布局零成本。
- 捆绑 Windows 版 xorriso：msys2 包的 DLL 依赖拓扑未核实，且本机与 CI 都没有
  可验证的 Windows 刻录环境，违背“无验证不交付”。GUI 的刻录能力与 CLI 等同：
  依赖 PATH 上的 xorriso，缺工具时报可行动的错误。

## 后果

- CI 增加 webkit2gtk 构建依赖与 node 22 工具链；根 workspace 的四目标检查不变。
- Windows 刻录仍需用户自装 xorriso（与 CLI 相同的已知限制），GUI 会给出提示。
