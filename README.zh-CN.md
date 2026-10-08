# optiburn

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

[简体中文](README.zh-CN.md) | [English](README.md)

## 简介

optiburn 是一个 Rust 写的跨平台光盘刻录工具，目标平台是 Linux 与 Windows 的
x86_64/arm64。它解决的核心问题是：在 Linux 上刻的盘，Windows 要能直接读。它先把
目录做成 Windows 能读的镜像（ISO 9660 + Joliet + UDF Bridge），再把镜像写到光驱。

## 当前能力（0.1.0）

| 命令 | 现状 |
|---|---|
| `optiburn build-image` | 可用。生成 ISO 9660 + Joliet + UDF Bridge 镜像，测试中与 xorriso 对拍验证。 |
| `optiburn burn` | Linux 可用，走 xorriso 子进程引擎。默认多区段（盘保持可追加），`--close-disc` 才封盘。 |
| `optiburn append` | Linux 可用。增长模式把目录追加成与已有区段合并的新区段，旧文件保持可见。 |
| `optiburn probe` | Linux 可用（`/dev/sr*`）。其它平台找不到设备时报“未发现光驱”（退出码 0），设备枚举目前只有 Linux 实现。 |

原生 MMC 写入（不借助外部工具）是下一个里程碑，不属于 0.1.0。分层设计见
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，介质与文件系统矩阵见
[docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md)。

## 环境要求

- Rust 1.98.1（由 `rust-toolchain.toml` 固定）。
- `xorriso` 在 `PATH` 上，只有 `burn` 需要，原因是 0.1.0 通过子进程调用它。
  Debian/Ubuntu 用 `sudo apt install xorriso` 安装，MSYS2 用 `pacman -S xorriso`。
- 对光驱的写权限：通常是加入 `cdrom` 组，或用 root。

## 安装

```bash
git clone https://github.com/ouyangjiahong26/optiburn
cd optiburn
cargo build --release
# 二进制：target/release/optiburn
```

## 用法

### 做镜像

```bash
$ optiburn build-image docs -o docs.iso --profile dvd --volume-id OPTIBURN
镜像: docs.iso
  扇区: 682
  字节: 1396736
  文件系统: ISO 9660, Joliet Level 3, UDF 1.02
```

（上面的输出来自本仓库的 `docs/` 目录。镜像内嵌构建时刻时间戳，所以字节数跟随
目录内容变化。）

`--profile` 决定写哪些文件系统：`cd`（无 UDF）、`dvd`（默认，UDF 1.02 Bridge）、
`bd`（UDF 2.50）。不给 `-o` 时，镜像写到当前目录，文件名是 `<源目录名>.iso`。

### 刻录

```bash
optiburn burn docs.iso --device /dev/sr0 --speed 8
```

Windows 的设备名是盘符：`--device E:`。默认盘保持可追加（多区段），`--close-disc`
才封盘。刻录前置检查自动处理介质状态：等盘片就绪（20 秒内每 500 ms 重试），已封口
的盘拒绝，可追加盘拒绝走镜像路径并提示改用 `append`（独立镜像会遮住已有区段的
文件），随机可写介质（DVD-RAM、BD-RE）直接放行。多区段说明见
[docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md)。目前唯一的引擎是 `xorriso`
（`--engine xorriso`，默认值），其它值报“未实现”错误，不会静默改用别的实现。

### 追加

```bash
optiburn append photos --device /dev/sr0 --volume-id PHOTOS
```

`append` 把目录内容映射到盘根，作为与已有区段合并的新区段追加：旧文件保持可见，
Windows 挂载最后一个区段就能看到全部文件。空盘上这等价于首刻。`--close-disc` 在
提交后封盘。对随机可写介质（DVD-RAM、BD-RE）不生效，xorriso 保持盘可覆写。

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
cargo test --workspace          # 回读对拍测试需要 xorriso
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
         x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
  RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --target "$t"
done
```

交叉目标上的死代码与未用导入由 rustc 报告，Linux 上的 clippy 看不到，所以这一步
同样拒绝警告，与 CI 的 cross-check 任务一致。

镜像生成测试会写出真实镜像，再用 xorriso 抽取回来逐字节比对，覆盖中文文件名与
两级子目录。缺 `xorriso` 时测试打印 `SKIP`。CI 会安装它，CI 里出现 `SKIP` 要当作
失败处理。

## 路线图

- v0（0.1.0）：镜像生成、xorriso 子进程引擎、多区段追加、只读 probe（本版本）。
- v0.5：在 `optiburn-transport` 之上做原生 MMC 写入，依次发 `RESERVE TRACK`、
  `WRITE(10)`、`SYNCHRONIZE CACHE`、`CLOSE TRACK`，不再依赖外部刻录工具。
- v0.6：刻录前的容量检查（`READ CAPACITY`、`GET CONFIGURATION`）与 Windows 上的
  probe。
- 更远：BD-R 伪覆写。

## 设计决策

- [ADR-0001](docs/adr/0001-pure-rust-per-os-scsi.md)：每个操作系统手写 SCSI
  传输层，不做 libburn FFI。
- [ADR-0002](docs/adr/0002-hadris-mastering.md)：用 hadris-cd 做镜像生成，不用
  libisofs 或 genisoimage。
- [ADR-0003](docs/adr/0003-image-first.md)：总是先产出镜像文件，再刻录。
- [ADR-0004](docs/adr/0004-xorriso-subprocess-v0.md)：xorriso 子进程作为 0.1.0
  的刻录引擎。
- [ADR-0005](docs/adr/0005-spti-windows.md)：Windows 上手写 SPTI 绑定，不用
  IMAPI2。
- [ADR-0006](docs/adr/0006-multi-session-default-and-grow.md)：默认多区段，追加
  走 xorriso 增长模式。
- [ADR-0007](docs/adr/0007-scsi-open-noblock.md)：打开 `/dev/sr*` 一律带
  `O_NONBLOCK`。

## 致谢

- [hadris](https://github.com/hxyulin/hadris)：MIT 许可的 Rust 镜像生成库
  （`hadris-cd`），UDF Bridge 镜像由它产出。
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/)：本项目测试对拍的
  参考实现，也是 0.1.0 的刻录后端（子进程调用，不链接 GPL 代码）。
- [alight](https://github.com/vicr123/alight)：Linux `SG_IO` 传输层结构的参考，
  未复制代码（该仓库没有许可证）。

## 许可

MIT，见 [LICENSE](LICENSE)。
