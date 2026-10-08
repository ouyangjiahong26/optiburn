# optiburn

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

[English](README.md) | [简体中文](README.zh-CN.md)

Cross-platform optical disc burning toolkit in Rust for Linux and Windows on
x86_64 and arm64. It builds disc images that Windows can read (ISO 9660 + Joliet
+ UDF Bridge) and writes them to a drive.

## What it does today (0.1.0)

| Command | State |
|---|---|
| `optiburn build-image` | Works: builds ISO 9660 + Joliet + UDF Bridge images. Verified against xorriso in tests. |
| `optiburn burn` | Works on Linux through the xorriso subprocess engine. Multi-session by default (the disc stays appendable); --close-disc finalizes. |
| `optiburn append` | Works on Linux. Appends a directory as a merged session (grow mode), so files from earlier sessions stay visible. |
| `optiburn probe` | Works on Linux (`/dev/sr*`). On other platforms it finds no devices and reports `未发现光驱` (exit 0), because device enumeration is Linux-only so far. |

Native MMC writing (no external tools) is the next milestone, not part of v0.
See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the layered design and
[docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) for the media/filesystem matrix.

## Requirements

- Rust 1.98.1 (pinned by `rust-toolchain.toml`).
- `xorriso` on `PATH`, only for `burn`, and only because v0 shells out to it.
  `sudo apt install xorriso` on Debian/Ubuntu, `pacman -S xorriso` on MSYS2.
- Write access to the drive: usually membership in the `cdrom` group, or root.

## Install

```bash
git clone https://github.com/ouyangjiahong26/optiburn
cd optiburn
cargo build --release
# binary: target/release/optiburn
```

## Usage

### Build an image

```bash
$ optiburn build-image docs -o docs.iso --profile dvd --volume-id OPTIBURN
镜像: docs.iso
  扇区: 682
  字节: 1396736
  文件系统: ISO 9660, Joliet Level 3, UDF 1.02
```

(The output above comes from this repository's `docs/` directory; the image
embeds a build timestamp, so sizes track the directory contents.)

`--profile` picks the filesystems: `cd` (no UDF), `dvd` (default, UDF 1.02
Bridge), `bd` (UDF 2.50). Without `-o` the image is written to
`<source-directory-name>.iso` in the current directory.

### Burn it

```bash
optiburn burn docs.iso --device /dev/sr0 --speed 8
```

Windows device names are drive letters: `--device E:`. By default the disc is
left appendable (multi-session); pass `--close-disc` to finalize it. See
[docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) for the multi-session notes.
The only engine is `xorriso`
(`--engine xorriso`, the default); other values fail with an explicit
"not implemented" error rather than silently doing something else.

### Append to a disc

```bash
optiburn append photos --device /dev/sr0 --volume-id PHOTOS
```

`append` appends the directory contents at the disc root as a merged new
session: files from earlier sessions stay visible, and Windows (which mounts
the last session) sees all of them. On a blank disc this is equivalent to
the first burn. `--close-disc` finalizes after committing; on randomly
writable media (DVD-RAM, BD-RE) it has no effect, and xorriso keeps the disc
rewritable.

### Inspect a drive

```bash
$ optiburn probe
未发现光驱                      # no drive present (exit code 0)
# with a drive, one line per device:
# /dev/sr0 | ASUS BW-16D1HT 3.10 | 空盘，1 个区段
```

Exits 0 even when no drive is present (`未发现光驱`), so it is safe to run in
scripts.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # needs xorriso for the round-trip test
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
         x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
  RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --target "$t"
done
```

Dead code and unused imports on the cross targets are reported by rustc but not
by clippy on Linux, so this pass denies warnings too, which is exactly how CI's
`cross-check` job runs.

The mastering test writes a real image and asks `xorriso` to extract it back,
then compares every file byte for byte, including a Chinese filename and a
two-level subdirectory. If `xorriso` is missing the test prints a `SKIP` line;
CI installs it, so a skip there is a failure to notice.

## Roadmap

- v0 (0.1.0): image mastering, xorriso subprocess engine, multi-session
  append, read-only probe (this).
- v0.5: native MMC writing on top of `optiburn-transport`, sending
  `RESERVE TRACK`, `WRITE(10)`, `SYNCHRONIZE CACHE` and `CLOSE TRACK` in
  sequence, no external burn tool needed.
- v0.6: capacity checks before burning (`READ CAPACITY`,
  `GET CONFIGURATION`), and `probe` on Windows.
- Later: BD-R pseudo-overwrite.

## Design decisions

- [ADR-0001](docs/adr/0001-pure-rust-per-os-scsi.md): per-OS SCSI transport
  written by hand, no libburn FFI.
- [ADR-0002](docs/adr/0002-hadris-mastering.md): hadris-cd for mastering, no
  libisofs or genisoimage.
- [ADR-0003](docs/adr/0003-image-first.md): always build an image file first,
  then burn it.
- [ADR-0004](docs/adr/0004-xorriso-subprocess-v0.md): xorriso subprocess as
  the v0 burn engine.
- [ADR-0005](docs/adr/0005-spti-windows.md): hand-written SPTI bindings on
  Windows, not IMAPI2.
- [ADR-0006](docs/adr/0006-multi-session-default-and-grow.md): multi-session
  by default, appends via xorriso grow mode.
- [ADR-0007](docs/adr/0007-scsi-open-noblock.md): `O_NONBLOCK` when opening
  `/dev/sr*`.

## Acknowledgements

- [hadris](https://github.com/hxyulin/hadris): the MIT-licensed Rust image
  writer (`hadris-cd`) that produces the UDF Bridge images.
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/): the reference
  implementation this project tests against, and the v0 burn backend
  (invoked as a subprocess; no GPL code is linked).
- [alight](https://github.com/vicr123/alight): reference for how a Linux
  `SG_IO` transport is structured; no code copied (that repository carries no
  license).

## License

MIT, see [LICENSE](LICENSE).

## 中文速览

完整中文文档见 [README.zh-CN.md](README.zh-CN.md)。

optiburn 是一个 Rust 写的跨平台光盘刻录工具，目标平台是 Linux 与 Windows 的
x86_64/arm64。它解决的核心问题是：在 Linux 上刻的盘，Windows 要能直接读。镜像同时写 ISO 9660、Joliet 与 UDF Bridge（见 `docs/WINDOWS-COMPAT.md`）。

四个命令：

- `optiburn build-image <目录> -o out.iso --profile dvd`：把目录做成镜像，
  `--profile` 取 `cd`/`dvd`/`bd`，决定写哪些文件系统。
- `optiburn burn out.iso --device /dev/sr0`：把镜像写到盘上（通过 xorriso 子进程）。
  默认多区段不封盘，`--close-disc` 才封盘。
- `optiburn append <目录> --device /dev/sr0`：把目录追加到盘上（xorriso 增长模式），
  与已有区段合并，旧文件保持可见。空盘时等价于首刻。
- `optiburn probe`：列出光驱与盘片状态（设备枚举只有 Linux 实现，其它平台一律报“未发现光驱”，退 0）。

刻录前置检查自动处理介质状态：等盘片就绪（20 秒内重试），已封口的盘拒绝，可追加盘
拒绝走镜像路径并提示用 `append`（独立镜像会遮住已有区段的文件）。

现状与边界：镜像层已实测（测试里用 xorriso 回读并逐字节比对，含中文文件名）。
刻录依赖 `xorriso`。probe 的设备枚举只在 Linux 实现，其它平台报“未发现光驱”，
真枚举与原生 MMC 写入引擎都在路线图上，v0 不留空壳。设计与取舍见 `docs/`、`docs/adr/`、`CONTEXT.md`。
