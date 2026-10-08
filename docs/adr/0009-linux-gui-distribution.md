# ADR-0009：图形前端的 Linux 分发用 AppImage 与 deb

- 状态：已接受
- 日期：2026-10-08

## 背景

ADR-0008 只定义了 Windows 分发（NSIS）。用户需要 Linux 上也有图形入口，且
x86_64 与 aarch64 都要支持。GUI 代码零平台分支，CI 的 gui job 已在 ubuntu 上
编译并测试，缺的只是打包与发布链。

## 决策

Linux 发两种包：AppImage（单文件免安装，任意发行版可跑）与 deb（Debian 系原生
安装，arm64 上树莓派系统是主要场景）。x86_64 在 ubuntu-latest 构建，aarch64 在
公开仓库免费的 ubuntu-24.04-arm 原生 runner 构建。Windows 维持 NSIS 不变。
刻录仍依赖 PATH 上的 xorriso，两种包都不内嵌外部工具。

## 被否决的方案

- 只发 deb：Arch、Fedora 用户没有免安装入口，放弃 AppImage 的单文件通用性。
- 加 rpm：首跑风险与资产维护面再涨一档，等有 Fedora 用户反馈再议。
- AppImage 内嵌 xorriso：许可上可行（GPL 工具子进程调用），但各发行版自带
  xorriso，内嵌只增加包体与更新负担。

## 后果

- release.yml 多一个 build-gui-linux job（矩阵 x86_64 与 aarch64），release
  job 的 needs 随之扩展。Release 资产从 6 项增至 10 项。
- AppImage 打包在构建期联网拉 linuxdeploy 工具（tauri CLI 自动处理）。
- tauri.conf.json 的 bundle.targets 维持 ["nsis"] 不变：发布与本地构建都显式
  传 --bundles，与 Windows job 的既有模式一致。
