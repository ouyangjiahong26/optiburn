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

## 补记：MSYS2 的 xorriso 没有光驱访问（2026-10-10）

首次在 Windows 上实测（USB 光驱，盘符 D:，盘片可追加）：`pacman -S xorriso` 装到的
构建（1.5.8.2-1）不含 MMC 传输层，`xorriso -devices` 报
`No MMC transport adapter is present. Running on sg-dummy.c.` 与 `No drives found`；
`-indev D:` 会落进 libburn 的 stdio 伪设备（Drive type 报 `YOYODYNE WARP DRIVE`）。

原因在上游构建逻辑：cygwin*/mingw* 上 libburn 唯一的 MMC 通道是 libcdio
（`configure.ac`：`cygwin*|mingw*) default_libcdio=yes`），而 MSYS2 的 PKGBUILD 既没
装 libcdio-devel 也没开 `--enable-libcdio`，配置阶段退回 sg-dummy。附带一条：该构建
按 msys 语义解析路径，`C:\…` 形态的绝对路径会被当成相对路径，连镜像文件操作也不适配
本仓库传参的方式（`optiburn-mastering` 的回读对拍在 Windows 上因此跑不动）。

结论：本文“缺工具时报可行动的错误”在当前 Windows 上无路可给，没有可用的外部引擎。
Windows 的刻录与读盘要等原生 MMC 引擎（v0.5）与原生读盘能力；缺工具文案不再给出
Windows 安装指引。若将来要捆绑外部引擎，必须是链接 libcdio 的原生 Windows 构建，
MSYS2 的包不行。README 的依赖说明与 ARCHITECTURE 的平台表已同步。
