# optiburn

![OptiBurn](assets/banner.png)

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

[English](README.md) | [简体中文](README.zh-CN.md)

Cross-platform optical disc burning toolkit in Rust for Windows and Linux on x86_64 and arm64. Software covering all four combinations is rare, and burning on Linux desktops (Ubuntu, Kylin, and other distributions) is especially rough: cryptic command-line tools, no progress, English-only errors, and disc states you have to sort out by hand. optiburn turns that into a reliable workflow: pre-flight disc checks, live progress, cancellable jobs, error causes classified into readable Chinese, and read-back verification. Images cover ISO 9660, Joliet and UDF Bridge, so older systems and Windows can read them directly.

## What it does today (0.1.3)

| Command | State |
|---|---|
| `optiburn build-image` | Works: builds ISO 9660 + Joliet + UDF Bridge images, verified against xorriso in tests. |
| `optiburn burn` | Works on Linux (Windows builds and enumerates drives, not yet drive-verified). Multi-session by default (the disc stays appendable); `--close-disc` finalizes. |
| `optiburn append` | Works on Linux (Windows not yet drive-verified). Appends a directory as a merged session; files from earlier sessions stay visible. |
| `optiburn probe` | Works on Linux (`/dev/sr*`) and Windows (drive letters). No drives found reports `未发现光驱` (exit 0). |
| OptiBurn GUI | New in 0.1.1 (Tauri 2 + React). Four pages mirror the four subcommands; burn tasks can be cancelled mid-write. The device page browses a disc's file tree with drag and Ctrl/Shift multi-select and copies files straight to the system clipboard, the append page burns a list of files picked in the file dialog or pasted with Ctrl+V, it inherits the disc's current label, both disc pages can read the disc back and compare it with the source, and writes to a mounted disc are blocked with an unmount hint. Clipboard copy and the mount guard are Linux-only for now and the GUI has not been drive-verified on Windows. Windows NSIS installers (x64, arm64) and Linux AppImage/deb packages (x86_64, arm64) ship from the releases page. |

Native MMC writing (no external tools) is the next milestone, not part of 0.1.3. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the layered design and [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) for the media/filesystem matrix.

## Install

```bash
git clone https://github.com/ouyangjiahong26/optiburn
cd optiburn
cargo build --release
# binary: target/release/optiburn
```

- Rust 1.98.1 (pinned by `rust-toolchain.toml`).
- `xorriso` on `PATH`, only for `burn`, and only because 0.1.3 shells out to it: `sudo apt install xorriso` on Debian/Ubuntu, `pacman -S xorriso` on MSYS2.
- Write access to the drive: usually membership in the `cdrom` group, or root.
- GUI: on Windows grab `OptiBurn_0.1.3_x64-setup.exe` (or the arm64 build). On Linux grab the AppImage (make it executable and run) or the `.deb`. Burning still needs `xorriso` on `PATH`, same as the CLI.

## Usage

### Build an image

```bash
$ optiburn build-image docs -o docs.iso --profile dvd --volume-id OPTIBURN
镜像: docs.iso
  扇区: 682
  字节: 1396736
  文件系统: ISO 9660, Joliet Level 3, UDF 1.02
```

The image embeds a build timestamp, so sizes track the directory contents. `--profile` picks the filesystems: `cd` (no UDF), `dvd` (default, UDF 1.02 Bridge), `bd` (UDF 2.50). Without `-o` the image is written to `<source-directory-name>.iso` in the current directory.

### Burn it

```bash
optiburn burn docs.iso --device /dev/sr0 --speed 8
```

Windows device names are drive letters: `--device E:`. By default the disc is left appendable (multi-session); pass `--close-disc` to finalize it. See [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) for the multi-session notes. The only engine is `xorriso` (`--engine xorriso`, the default); other values fail with an explicit "not implemented" error.

### Append to a disc

```bash
optiburn append photos --device /dev/sr0 --volume-id PHOTOS
```

`append` appends the directory contents at the disc root as a merged new session: files from earlier sessions stay visible, and Windows (which mounts the last session) sees all of them. On a blank disc this is equivalent to the first burn. `--close-disc` finalizes after committing; on randomly writable media (DVD-RAM, BD-RE) it has no effect, and xorriso keeps the disc rewritable.

### Inspect a drive

```bash
$ optiburn probe
未发现光驱                      # no drive present (exit code 0)
# with a drive, one line per device:
# /dev/sr0 | ASUS BW-16D1HT 3.10 | 空盘，1 个区段
```

Exits 0 even when no drive is present (`未发现光驱`), so it is safe to run in scripts.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # needs xorriso; the round-trip test compares byte for byte
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
         x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
  RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --target "$t"
done
```

## Acknowledgements

- [hadris](https://github.com/hxyulin/hadris): the MIT-licensed Rust image writer (`hadris-cd`) that produces the UDF Bridge images.
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/): the reference implementation this project tests against, and the 0.1.3 burn backend (invoked as a subprocess; no GPL code is linked).
- [alight](https://github.com/vicr123/alight): reference for how a Linux `SG_IO` transport is structured; no code copied (that repository carries no license).

## License

MIT, see [LICENSE](LICENSE).

