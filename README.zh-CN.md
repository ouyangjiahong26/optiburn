# optiburn

![OptiBurn](assets/banner.png)

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

[简体中文](README.zh-CN.md) | [English](README.md)

optiburn 是一个 Rust 写的跨平台光盘刻录工具，目标平台是 Windows 与 Linux 的 x86_64 与 arm64。它做三件事：把目录做成镜像，把镜像或新区段写到光驱，把盘上的内容读回来。镜像带 ISO 9660 与 Joliet，`dvd` 与 `bd` 档另带 UDF（`dvd` 是 UDF 1.02 Bridge），Windows 与老设备都能直接读。

同时支持 Windows 与 Linux、x86_64 与 arm64 四个组合的刻录软件很少，Linux 桌面（Ubuntu、银河麒麟等发行版）上的体验尤其粗糙：命令行参数难记，没有进度，失败只有英文报错，盘片状态不对时要自己排查。optiburn 把写前检查、可中止的实时进度、归类的失败说明、回读校验放在同一条流水线上。

## 当前能力（0.1.9）

| 命令 | 做什么 | 平台 |
|---|---|---|
| `optiburn build-image` | 按 profile 生成 ISO 9660、Joliet、UDF 镜像。字节布局在测试里逐扇区断言，并与 xorriso 对拍。 | 四个目标都可用。 |
| `optiburn burn` | 把镜像写到盘。介质支持时默认多区段。`--close-disc` 由引擎封盘，原生封盘只覆盖 CD 与 DVD-R。 | Linux 默认 `xorriso`，可选 `--engine native`。Windows 只走原生。 |
| `optiburn append` | 读出末区段，把源目录嫁接上去，新区段只写目录结构与新文件数据，旧文件的数据块原地引用。 | 两平台默认原生增长。Linux 在随机可写介质、未知 Profile 或指定倍速时回退 xorriso 增长模式。 |
| `optiburn probe` | 列出光驱、盘片状态、容量、增长模式后端。没有光驱时退出 0。 | Linux 的 `/dev/sr*`，Windows 的设备路径（`E:`）。 |
| `optiburn repair` | 关掉中断刻录留下的损坏轨道与区段，盘可以继续写。 | Linux 走 xorriso 的 `-close_damaged`，Windows 走原生。 |
| `optiburn salvage` | 读标准读取器看不见的未关闭轨道，按写入边界给每条文件定性，把能读的部分抽出来。 | 两平台都走原生 MMC 读侧。先抢救，再修复。 |
| 读盘 | 卷标、末区段门禁、目录树列举、抽取收在同一个 `ReadBackend` 接缝上。 | Linux 先走 xorriso，读不到 ISO 9660 时回退原生。Windows 走原生 ISO 9660 与 UDF。 |
| 图形前端 | 四个页面覆盖 `probe`、`build-image`、`burn`、`append`、`salvage`，`repair` 只在命令行。刻录类任务可中止。 | Tauri 2 + React。Windows 的刻录、增长、读盘走原生，Linux 的刻录与读盘首路径走 xorriso，追加默认原生增长。 |

硬件证据与功能清单分开列，避免把验证范围读成功能范围：

- 原生刻录在 2026-10-10 的 Windows CD-R 与 2026-10-22 的 Linux DVD-R 上真机验证，后者用了两台驱动器。DVD+R、DVD+RW、BD-R 还没有真机写入验证。
- 原生增长在 2026-10-15 的 Linux 多区段 CD-R 上实测，写前从 51.0 秒降到 1.6 秒。DVD+R 与 DVD+RW 的追加仍未验证，DVD-R 与 DVD-RW 做不出第二区段。
- 读盘在 2026-10-10 的 Windows CD-R 上验证，两种区段地址约定都读通。Linux 的后端回退在 2026-10-12 的设备路径上验证，纯 UDF 实物盘仍待补。
- `salvage` 在 2026-10-11 的一张被中断写入的盘上验证。

每条结论都有 ADR 或测试支撑，代码注释记下上游怪癖。接缝与数据流见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，介质与文件系统矩阵见 [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md)。

## 安装

### 命令行

依赖：

- Rust 1.98.1，由 `rust-toolchain.toml` 固定。
- `PATH` 上有 `xorriso`，Linux 的默认刻录引擎、读盘首路径、增长模式回退、测试参考实现都用它。Debian 与 Ubuntu 用 `sudo apt install xorriso`。
- 对光驱的写权限，通常加入 `cdrom` 组，或用 root。

```bash
git clone https://github.com/ouyangjiahong26/optiburn
cd optiburn
cargo build --release
# 二进制：target/release/optiburn
```

Windows 不需要外部工具，刻录、增长与读盘都走原生路径。那里也没有可用的 xorriso 构建：MSYS2 的包编译时未链 libcdio，只能操作镜像文件，碰不到光驱（2026-10-10 实测，证据见 ADR-0008 补记）。

### 图形前端

Windows 下载 `OptiBurn_0.1.9_x64-setup.exe` 或 arm64 版，Linux 下载 AppImage（`chmod +x` 后直接运行）或 `.deb` 安装包。无网的 Windows 机器改用 x64 安装包的 `-offline` 版本：内嵌 WebView2 运行时，安装器约 210 MB，安装全程不需要联网。缺补丁的 Windows 7 上 WebView2 安装器会报 0x8007007F，先从 [Microsoft Update Catalog](https://www.catalog.update.microsoft.com/Search.aspx?q=KB2533623) 离线安装 KB2533623 与 KB3063858 再安装（详见 ADR-0016 补记）。

## 用法

### 做镜像

```bash
$ optiburn build-image docs -o docs.iso --profile dvd --volume-id OPTIBURN
镜像: docs.iso
  扇区: 682
  字节: 1396736
  文件系统: ISO 9660, Joliet Level 3, UDF 1.02
```

镜像内嵌构建时刻时间戳，所以字节数跟随目录内容变化。`--profile` 决定写哪些文件系统：`cd` 不写 UDF，`dvd` 是默认值并写 UDF 1.02 Bridge，`bd` 写 UDF 2.50。不给 `-o` 时，镜像写到当前目录，文件名是 `<源目录名>.iso`。

### 刻录

```bash
optiburn burn docs.iso --device /dev/sr0 --speed 8
```

Windows 的设备路径是 `E:`。默认不主动封盘，能否继续追加取决于介质。`--close-disc` 由引擎封盘。Linux 默认的 xorriso 能封它支持的介质，DVD+R 与 BD-R 也在其中。原生引擎只实现 CD 与 DVD-R 的封盘，所以 DVD+R 与 BD-R 在 Windows 上不封盘，Linux 上需要 xorriso（ADR-0017）。写前检查会等盘片就绪，拒绝已封口的盘，拒绝在可追加盘上写整份镜像并提示改用 `append`，放行随机可写介质，Linux 上盘被系统挂载时拦下并提示卸载。

`--engine` 取 `xorriso`（默认值，需要 `PATH` 上有 `xorriso`）或 `native`（自己发 MMC 命令，Windows 上唯一可用），其它值直接拒绝。原生引擎在可追加盘上写新区段，拒绝已封口介质、不支持的 Profile、空镜像、放不下的镜像。它暂不支持指定倍速，用 `--engine native` 时把 `--speed` 留空。

### 追加

```bash
optiburn append photos --device /dev/sr0 --volume-id PHOTOS
```

`append` 把目录内容映射到盘根，作为与已有区段合并的新区段追加。旧文件保持可见，Windows 挂载最后一个区段就能看到全部文件。空盘上这等价于首刻。`--close-disc` 在提交后由引擎封盘。原生引擎支持 CD 与 DVD-R，DVD+R 与 BD-R 需要 Linux 上的 xorriso 后端。

对随机可写介质（DVD-RAM、BD-RE），原生增长拒绝写入。Linux 默认档回退 xorriso，在那里 `--close-disc` 不生效，盘保持可覆写。

原生增长生成 ISO 9660 加 Joliet 的区段，旧文件的数据块原地引用（ADR-0020）。资源管理器要弹出光盘再放回才会刷新挂载视图。旧区段形状不支持嫁接时（没有 Joliet、含启动记录、含多 extent 文件），追加会被拒绝并说明原因。

Linux 只按驱动器的静态事实与调用方的请求回退 xorriso：随机可写介质、未知 Profile、指定了倍速。图形前端可选增长模式后端，命令行用 `OPTIBURN_GROW_BACKEND`（`auto`、`native`、`xorriso`），显式选择不回退。

### 查看光驱

```bash
$ optiburn probe
未发现光驱                      # 没有光驱（退出码 0）
# 有光驱时每个设备一行：
# /dev/sr0 | ASUS BW-16D1HT 3.10 | 空盘，1 个区段
```

没有光驱时也退出 0（报“未发现光驱”），可以放心放进脚本。

### 抢救与修复

```bash
optiburn salvage --device D: --dest C:\rescued
optiburn repair --device D:
```

先跑 `salvage`（Linux 上 `--device` 换成 `/dev/sr0`）。它只读盘面，把未关闭轨道里的每条文件按写入边界定性为完整、半截或一个字节都没写，再把能读的部分按盘上路径抽出来。半截的 `.zip` 只保留完整条目并重建中央目录与 EOCD，常规解压工具能直接打开。不需要源文件：边界与条目完整性都由盘上结构自证。

`repair` 按 libburn 的 `burn_disc_close_damaged` 顺序关掉损坏的轨道与区段，盘可以继续写。它会消掉 `salvage` 依赖的未关闭轨道入口，所以顺序不能反。两条路径都记在 ADR-0022。

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

## 延伸阅读

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)：crate 边界、数据流与各接缝的不变量。
- [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md)：介质与文件系统矩阵、多区段规则、验证步骤。
- [docs/MULTI-DRIVE.md](docs/MULTI-DRIVE.md)：一次写多台光驱，以及“一台设备只从一个入口发起”的规则。
- [文档站](https://ouyangjiahong26.github.io/optiburn/)：面向最终用户的说明文档。
- [CONTEXT.md](CONTEXT.md)：代码、测试与 issue 共用的领域术语表。

## 致谢

- [hadris](https://github.com/hxyulin/hadris)：MIT 许可的 Rust 镜像生成库（`hadris-cd`），ISO 9660、Joliet、UDF 布局由它产出。
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/)：本项目测试对拍的参考实现，也是 Linux 的默认刻录后端（子进程调用，不链接 GPL 代码）。
- [alight](https://github.com/vicr123/alight)：Linux `SG_IO` 传输层结构的参考，未复制代码（该仓库没有许可证）。

## 许可

MIT，见 [LICENSE](LICENSE)。
