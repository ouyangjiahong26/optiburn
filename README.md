# optiburn

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

Cross-platform optical disc burning toolkit in Rust for Linux and Windows on
x86_64 and arm64. It builds disc images that Windows can read (ISO 9660 + Joliet
+ UDF Bridge) and writes them to a drive.

## What it does today (v0)

| Command | State |
|---|---|
| `optiburn build-image` | Works: builds ISO 9660 + Joliet + UDF Bridge images. Verified against xorriso in tests. |
| `optiburn burn` | Works on Linux through the `xorriso -as cdrecord` subprocess engine. |
| `optiburn probe` | Works on Linux (`/dev/sr*`). Not implemented on Windows yet. |

Native MMC writing (no external tools) is the next milestone, not part of v0.
See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the layered design and
[docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) for the media/filesystem matrix.

## Requirements

- Rust 1.98.1 (pinned by `rust-toolchain.toml`).
- `xorriso` on `PATH` — only for `burn`, and only because v0 shells out to it.
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

Windows device names are drive letters: `--device E:`. `--multi` appends as a
new session instead of closing the disc; read the multi-session notes in
[docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) before using it, because
Windows only mounts the last session. The only engine in v0 is `xorriso`
(`--engine xorriso`, the default); other values fail with an explicit
"not implemented" error rather than silently doing something else.

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
by clippy on Linux, so this pass denies warnings too — which is exactly how CI's
`cross-check` job runs.

The mastering test writes a real image and asks `xorriso` to extract it back,
then compares every file byte for byte — including a Chinese filename and a
two-level subdirectory. If `xorriso` is missing the test prints a `SKIP` line;
CI installs it, so a skip there is a failure to notice.

## Roadmap

- **v0** — image mastering, xorriso subprocess engine, read-only probe (this).
- **v0.5** — native MMC writing on top of `optiburn-transport`:
  `RESERVE TRACK` → `WRITE(10)` → `SYNCHRONIZE CACHE` → `CLOSE TRACK`, no
  external burn tool needed.
- **v0.6** — capacity checks before burning (`READ CAPACITY`,
  `GET CONFIGURATION`); `probe` on Windows.
- Later: multi-session appends, BD-R pseudo-overwrite.

## Design decisions

- [ADR-0001](docs/adr/0001-pure-rust-per-os-scsi.md) — per-OS SCSI transport
  written by hand, no libburn FFI.
- [ADR-0002](docs/adr/0002-hadris-mastering.md) — hadris-cd for mastering, no
  libisofs or genisoimage.
- [ADR-0003](docs/adr/0003-image-first.md) — always build an image file first,
  then burn it.
- [ADR-0004](docs/adr/0004-xorriso-subprocess-v0.md) — xorriso subprocess as
  the v0 burn engine.
- [ADR-0005](docs/adr/0005-spti-windows.md) — hand-written SPTI bindings on
  Windows, not IMAPI2.

## Acknowledgements

- [hadris](https://github.com/hxyulin/hadris) — the MIT-licensed Rust image
  writer (`hadris-cd`) that produces the UDF Bridge images.
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/) — the reference
  implementation this project tests against, and the v0 burn backend
  (invoked as a subprocess; no GPL code is linked).
- [alight](https://github.com/vicr123/alight) — reference for how a Linux
  `SG_IO` transport is structured; no code copied (that repository carries no
  license).

## License

MIT — see [LICENSE](LICENSE).

## 中文速览

**optiburn** 是一个 Rust 写的跨平台光盘刻录工具，目标平台是 Linux 与 Windows 的
x86_64/arm64。它解决的核心问题是：**在 Linux 上刻的盘，Windows 要能直接读**——
镜像同时写 ISO 9660、Joliet 与 UDF Bridge（见 `docs/WINDOWS-COMPAT.md`）。

三个命令：

- `optiburn build-image <目录> -o out.iso --profile dvd`：把目录做成镜像，
  `--profile` 取 `cd`/`dvd`/`bd`，决定写哪些文件系统；
- `optiburn burn out.iso --device /dev/sr0`：把镜像写到盘上（v0 通过 xorriso 子进程）；
- `optiburn probe`：列出光驱与盘片状态（Linux；无光驱时退 0）。

现状与边界：镜像层已实测（测试里用 xorriso 回读并逐字节比对，含中文文件名）；
刻录依赖 `xorriso`；`probe` 只支持 Linux；原生的 MMC 写入引擎与 Windows 的 `probe`
在路线图上，v0 不留空壳。设计与取舍见 `docs/`、`docs/adr/`、`CONTEXT.md`。
