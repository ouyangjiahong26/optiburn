# ADR-0016：Windows 离线安装包（内嵌 WebView2 Standalone）

- 状态：已接受
- 日期：2026-10-10
- 受影响模块：`.github/workflows/release.yml`、`src-tauri/tauri.offline.conf.json`

## 背景

默认的 NSIS 安装包用 `downloadBootstrapper` 装 WebView2：安装时在线下载运行时。
实机反馈：Windows 7 上安装需要联网拉 WebView2，无网机器装不了（issue 来自一台
无网的 Win7 测试机）。

## 决策

1. 常规安装包维持 `downloadBootstrapper` 不变。绝大多数 Win10/11 机器自带
   WebView2（安装器检测到即跳过），保持小体积（安装包本体约 7 MB）。
2. 追加 x64 离线安装包 `OptiBurn_<版本>_x64-setup-offline.exe`：用
   `--config src-tauri/tauri.offline.conf.json` 覆盖 `webviewInstallMode` 为
   `offlineInstaller` 二次打包，内嵌 WebView2 Standalone（安装器约 210 MB），安装全程
   不需要网络。tauri-bundler 打包时从微软官方地址下载 Standalone 嵌入。
3. Windows 7 的版本上限由微软的安装器自己处理：WebView2 已于 2023-01-10 结束
   对 Win7 的支持，最后的兼容版本是 109；Evergreen Standalone 安装器有版本感知，
   在 Win7 上自动安装 109 而不是最新版。因此嵌入“最新 Standalone”对 Win7 离线
   机器同样正确，不需要钉死特定版本。
4. 只做 x64：Win7 没有 arm64 机器，arm64 的离线需求出现时在同一矩阵腿加同样
   两条步骤即可。用户语境里的“x86 系统的安装包”指 Intel/AMD 架构（我们只发
   x64 安装包）；真正的 32 位 Windows 需要 i686 目标，属于另一个话题，本决策
   不涉及。
5. 离线安装包不进 latest.json：updater 的产物匹配以 `_x64-setup.exe` 结尾，
   离线版的 `-offline.exe` 后缀天然不匹配，应用内更新始终下载常规安装包。

## 后果

- Release 每个 x64 版本多一个约 210 MB 的资产，CI 的 Windows GUI job 多一次
  NSIS 打包（Rust 编译有增量缓存，多出的主要是打包与下载 Standalone 的时间）。
- 常规安装包构建后必须先收进 bundles：离线构建在 bundle/nsis/ 产出同名文件，
  顺序错了常规包会被覆盖成离线包。
- 已知风险：Tauri 不代装 VC++ 运行库，缺 UCRT 的裸 Win7 上应用本体仍可能起不来
 （这与 WebView2 无关）。若真机验证中招，后续在 NSIS 模板里补 VC redist 安装，
  不在本决策范围内。

## 补记：v0.1.5 裸 Win7 实测（2026-10-10）

离线安装包在一台缺补丁的 Windows 7 上失败：NSIS 日志显示 WebView2 安装器报
错误码 `-2147024769`（0x8007007F，"找不到指定的过程"）。这不是离线包的问题，
而是 Standalone 安装器自身要调的 API 在裸 Win7 上不存在，社区共识的根因是缺
KB2533623 与 KB3063858（KB3063858 是 Win7 的 UCRT 相关更新，KB2533623 补
kernel32 导出）。处理方式：先从 Microsoft Update Catalog 离线取得这两个 KB
装上，再跑离线安装包（README 的离线安装说明同步写了这一步）。

考虑过改用 `fixedRuntime`（把 109 固定版本运行时打进应用目录，完全不跑
WebView2 安装器）：109 是最后支持 Win7 的运行时，但微软开发者页的 Fixed
Version 只保留最近五个版本（2026-10 实测为 151 至 155），109 的官方 cab 直链
已下架，没有可复核的官方来源，不采纳。若未来拿到可信来源或 Win7 需求升级，
再评估。
