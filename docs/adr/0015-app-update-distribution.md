# ADR-0015：应用更新能力：AppImage 增量更新与应用内更新双通道

- 状态：已接受
- 日期：2026-10-10
- 受影响模块：`src-tauri/`、`frontend/`、`.github/workflows/release.yml`、`packaging/`

## 背景

0.1.3 起通过 GitHub Release 分发安装包，用户没有任何更新机制：AppImageHub 收录
测试（AppImage/appimage.github.io#9948）给出 warning，AppImage 里没有嵌入式更新
信息，AppImageUpdate 类工具无从检查新版本；Windows NSIS 用户同样只能手动重下。

issue #27 要求补上更新能力。评估过两条独立路线：

- AppImage 原生 zsync：嵌入式更新信息加 `.zsync` 元数据，AppImageUpdate 据此做
  增量更新。生态标准做法，也是消除 AppImageHub warning 的唯一方式。
- Tauri updater（altgo 的 ADR-0004 已落地同款）：应用内检查更新，Ed25519 签名
  校验，Windows 与 Linux 一致体验。

两条路线服务不同入口（生态工具与应用内），互不冲突，一并落地。

## 决策

1. AppImage 嵌入更新信息并发布 `.zsync`。不做重打包后处理：tauri-bundler 调
   linuxdeploy 时不清洗环境（`Command::new` 继承父进程环境，只追加 `OUTPUT`、
   `ARCH`、`APPIMAGE_EXTRACT_AND_RUN`），在构建步骤设 `UPDATE_INFORMATION` 即可
   由 linuxdeploy 的 AppImage 输出插件嵌入更新信息并产出 `.zsync`
   （2026-10-10 用 tauri 固定的 linuxdeploy-07333c6 实测验证；注意 `.zsync` 落在
   linuxdeploy 的工作目录即 `src-tauri/`，不在 `OUTPUT` 同目录，流水线按此取用）。
   更新信息用
   `gh-releases-zsync|ouyangjiahong26|optiburn|latest|OptiBurn_*_<架构>.AppImage.zsync`，
   模式串里的架构段是资产名后缀（x86_64 腿是 `amd64`，aarch64 腿是 `aarch64`），
   不能直接用目标三元组的架构名。
2. 应用内更新走 `tauri-plugin-updater` 加 `tauri-plugin-process`（只用了它的
   relaunch）。更新源是 Release 附带的 `latest.json`（endpoints 指向
   `releases/latest/download/latest.json`），产物用 Ed25519 minisign 签名：公钥
   固化在 `tauri.conf.json`，私钥在 Actions Secret `TAURI_SIGNING_PRIVATE_KEY`。
   `createUpdaterArtifacts` 打开后，NSIS 的 updater 产物是签名后的安装包本体，
   AppImage 的是签名后的裸 AppImage（v2 原生行为，均伴生 `.sig`）。
   `latest.json` 由 `packaging/merge-updater-json.sh` 在发布 job 合成，tauri CLI
   只产出签名产物不出清单。
3. 分级开放应用内更新（`src-tauri/src/updater.rs` 的 `can_self_update`）：
   Windows 恒可用；Linux 仅 AppImage（运行时有 `APPIMAGE` 环境变量）；deb 安装
   的二进制归包管理器管，就地替换会破坏 dpkg 记录，入口直接不显示。写盘任务
   进行中入口禁用；下载完成时若任务仍在跑，不自动重启，把完成安装留给下一次
   启动（AppImage 的安装是替换文件，重启只是生效动作；Windows 上安装器会接管
   退出重启，确认对话框之后的窗口期极小，接受该残余风险）。
4. 签名密钥不设口令：私钥只存在于 GitHub Secret 与维护者本地备份
   （`~/.tauri/optiburn-updater.key`），丢失口令与丢失私钥等价，多一层口令只
   保护“Secret 泄漏且攻击者不会跑构建”的组合场景，不值得多管理一项。

## 后果

- `tauri build`（appimage/nsis 目标）现在要求 `TAURI_SIGNING_PRIVATE_KEY`，
  本地构建要显式给路径（见 AGENTS.md 开发命令）。
- Release 资产多了 `.sig`（四个安装包各一）与两个 `.zsync`、一个 `latest.json`；
  `SHA256SUMS.txt` 不收录 `.zsync`（它是给 AppImageUpdate 的元数据，不是人工
  核对对象）。
- v0.1.3 的存量 AppImage 没有嵌入式更新信息，AppImageUpdate 用户从 v0.1.4 起
  才能增量更新；updater 的版本比较同理从带 `latest.json` 的首个版本起生效。
- squashfs 不可复现，版本间字节差异大时 zsync 增量可能接近全量下载；机制仍成立，
  代价是流量不是断点兼容。
