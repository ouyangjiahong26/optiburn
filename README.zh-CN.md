# optiburn

![OptiBurn](assets/banner.png)

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

[简体中文](README.zh-CN.md) | [English](README.md)

## 简介

optiburn 是一个 Rust 写的跨平台光盘刻录工具，目标平台是 Windows 与 Linux 的 x86_64 与 arm64。

同时支持 Windows 与 Linux、x86_64 与 arm64 四个组合的刻录软件很少，而 Linux 桌面（Ubuntu、银河麒麟等发行版）上的刻录体验尤其粗糙：命令行参数难记、没有进度、失败只有英文报错，盘片状态不对时要自己慢慢排查。optiburn 把这条流水线做成可靠的工具：写前检查盘片状态，刻录有实时进度且可中止，失败成因归类成可读说明，写后可回读校验。生成的镜像覆盖 ISO 9660、Joliet 与 UDF Bridge，老设备与 Windows 都能直接读。

## 当前能力（0.1.5）

| 命令 | 现状 |
|---|---|
| `optiburn build-image` | 可用。生成 ISO 9660 + Joliet + UDF Bridge 镜像，测试中与 xorriso 对拍验证。 |
| `optiburn burn` | Linux 可用（`xorriso` 或 `--engine native`），Windows 可用（`--engine native`，图形前端默认，2026-10-10 在 CD-R 上真机验证，DVD/BD 各族待介质）。默认多区段（盘保持可追加），`--close-disc` 才封盘。 |
| `optiburn append` | Linux 可用（xorriso 增长模式），Windows 可用（原生增长模式，ADR-0020）：引擎读出末区段的目录树，把源目录嫁接上去，新区段只写目录结构与新文件数据，旧文件的数据块原地引用。追加目录并合并已有区段，旧文件保持可见。 |
| `optiburn probe` | Linux（`/dev/sr*`）与 Windows（盘符）可用。找不到设备时报“未发现光驱”（退出码 0）。 |
| 读盘 | 可用。读侧（卷标、末区段格式门禁、目录树列举、抽取）接在 `ReadBackend` 接缝上：Linux 走 xorriso，Windows 走原生 MMC 加 ISO 9660 与 UDF 解析（ADR-0018、ADR-0021，2026-10-10 在 CD-R 上真机验证，两种区段地址约定都读通）。 |
| `OptiBurn` 图形前端 | 0.1.1 新增（Tauri 2 + React）。四个页面对应四个子命令，刻录类任务可中止。设备页可展开盘上文件清单，支持拖动框选与 Ctrl、Shift 多选，选中后可一键复制到系统剪贴板供文件管理器粘贴（复制与挂载门禁目前只在 Linux 生效）。Windows 上刻录与读盘都走原生路径（不需要外部工具，读盘见 ADR-0018）。追加页的待刻录文件可用文件选择框多选，或在文件管理器复制后按 Ctrl+V 粘贴。卷标自动沿用盘上现有值，刻录与追加两页都能回读盘片并与源逐文件对比，盘被系统挂载时写入会被拦下并提示卸载。Windows 安装包（NSIS，x64 与 arm64）与 Linux 安装包（AppImage 与 deb，x86_64 与 arm64）从 Release 页下载。AppImage 内嵌更新信息，可用 AppImageUpdate 增量更新，图形界面在 Windows 安装包与 AppImage 上可就地检查更新。 |

原生 MMC 写入（不借助外部工具）已落地为 `--engine native` 与图形前端在 Windows 上的默认引擎（ADR-0017），原生读盘（ADR-0018）让 Windows 上设备页浏览、回读校验与卷标预填也不需要外部工具，原生增长模式（ADR-0020）让 Windows 上的追加与 Linux 一样可用，原生读侧的 UDF 支持（ADR-0021）让 Windows 写的 UDF 盘也能浏览与复制。DVD/BD 各族的真机验证仍待后续。分层设计见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，介质与文件系统矩阵见 [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md)。

## 安装

```bash
git clone https://github.com/ouyangjiahong26/optiburn
cd optiburn
cargo build --release
# 二进制：target/release/optiburn
```

- Rust 1.98.1（由 `rust-toolchain.toml` 固定）。
- `xorriso` 在 `PATH` 上：CLI 的默认刻录引擎（Linux）与图形前端在 Linux 上的盘上浏览、复制、回读校验、增长模式都用它，它也是测试里的参考实现。Debian/Ubuntu 用 `sudo apt install xorriso`。Windows 上没有可用的构建：MSYS2 的包（`pacman -S xorriso`）编译时未链 libcdio，只能操作镜像文件，碰不到光驱（2026-10-10 实测，证据见 ADR-0008 补记）。Windows 上刻录与读盘都不需要外部工具：图形前端与 `--engine native` 走原生 MMC 引擎（ADR-0017），读侧走原生解析（ADR-0018）。
- 图形前端：Windows 下载 `OptiBurn_0.1.5_x64-setup.exe`（或 arm64 版），Linux 下载 AppImage（`chmod +x` 后直接运行）或 `.deb` 安装包。刻录与读盘在 Windows 上都走原生路径（不需要外部工具），Linux 上走 `xorriso`（见上）。无网的 Windows 机器改用 x64 安装包的 `-offline` 版本：内嵌 WebView2 运行时（安装器约 210 MB），安装全程不需要联网。缺补丁的 Windows 7 上 WebView2 安装器会报 0x8007007F，先从 [Microsoft Update Catalog](https://www.catalog.update.microsoft.com/Search.aspx?q=KB2533623) 离线装 KB2533623 与 KB3063858 再安装（详见 ADR-0016 补记）。
- 对光驱的写权限：通常加入 `cdrom` 组，或用 root。

## 用法

### 做镜像

```bash
$ optiburn build-image docs -o docs.iso --profile dvd --volume-id OPTIBURN
镜像: docs.iso
  扇区: 682
  字节: 1396736
  文件系统: ISO 9660, Joliet Level 3, UDF 1.02
```

镜像内嵌构建时刻时间戳，所以字节数跟随目录内容变化。`--profile` 决定写哪些文件系统：`cd`（无 UDF）、`dvd`（默认，UDF 1.02 Bridge）、`bd`（UDF 2.50）。不给 `-o` 时，镜像写到当前目录，文件名是 `<源目录名>.iso`。

### 刻录

```bash
optiburn burn docs.iso --device /dev/sr0 --speed 8
```

Windows 的设备名是盘符：`--device E:`。默认盘保持可追加（多区段），`--close-disc` 才封盘。刻录前置检查自动处理介质状态：等盘片就绪（20 秒内每 500 ms 重试），已封口的盘拒绝，可追加盘拒绝走镜像路径并提示改用 `append`，随机可写介质（DVD-RAM、BD-RE）直接放行。多区段说明见 [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md)。`--engine` 取 `xorriso`（默认值，需要 `PATH` 上有 `xorriso`）或 `native`（自己发 MMC 命令，Windows 上唯一可用），其它值报“未实现”错误。原生引擎在可追加盘上写新区段，拒绝已封口介质、不支持的 Profile、空镜像、放不下的镜像，暂不支持指定倍速（倍速留空）。

### 追加

```bash
optiburn append photos --device /dev/sr0 --volume-id PHOTOS
```

`append` 把目录内容映射到盘根，作为与已有区段合并的新区段追加：旧文件保持可见，Windows 挂载最后一个区段就能看到全部文件。空盘上这等价于首刻。`--close-disc` 在提交后封盘。对随机可写介质（DVD-RAM、BD-RE）不生效，xorriso 保持盘可覆写。Windows 上追加走原生增长模式（ADR-0020），生成的区段是 ISO 9660 加 Joliet、旧文件数据块原地引用；除传 `--close-disc` 外盘保持可追加，资源管理器要弹出光盘再放回才会刷新挂载视图。

### 查看光驱

```bash
$ optiburn probe
未发现光驱                      # 没有光驱（退出码 0）
# 有光驱时每个设备一行：
# /dev/sr0 | ASUS BW-16D1HT 3.10 | 空盘，1 个区段
```

没有光驱时也退出 0（报“未发现光驱”），可以放心放进脚本。

## 验证

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # 需要 xorriso，回读对拍逐字节比较
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
         x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
  RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --target "$t"
done
```

## 致谢

- [hadris](https://github.com/hxyulin/hadris)：MIT 许可的 Rust 镜像生成库（`hadris-cd`），UDF Bridge 镜像由它产出。
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/)：本项目测试对拍的参考实现，也是 0.1.5 的刻录后端（子进程调用，不链接 GPL 代码）。
- [alight](https://github.com/vicr123/alight)：Linux `SG_IO` 传输层结构的参考，未复制代码（该仓库没有许可证）。

## 许可

MIT，见 [LICENSE](LICENSE)。
