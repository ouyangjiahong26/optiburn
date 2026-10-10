# optiburn

![OptiBurn](assets/banner.png)

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

[English](README.md) | [简体中文](README.zh-CN.md)

Cross-platform optical disc burning toolkit in Rust for Windows and Linux on x86_64 and arm64. Software covering all four combinations is rare, and burning on Linux desktops (Ubuntu, Kylin, and other distributions) is especially rough: cryptic command-line tools, no progress, English-only errors, and disc states you have to sort out by hand. optiburn turns that into a reliable workflow: pre-flight disc checks, live progress, cancellable jobs, error causes classified into readable messages, and read-back verification. Images cover ISO 9660, Joliet and UDF Bridge, so older systems and Windows can read them directly.

## What it does today (0.1.5)

| Command | State |
|---|---|
| `optiburn build-image` | Works: builds ISO 9660 + Joliet + UDF Bridge images, verified against xorriso in tests. |
| `optiburn burn` | Works on Linux (`xorriso`, or `--engine native`) and on Windows (`--engine native`, the GUI default; the native engine was verified on a real CD-R on 2026-10-10, DVD/BD media still untested). Multi-session by default (the disc stays appendable); `--close-disc` finalizes. |
| `optiburn append` | Works on Linux via xorriso grow mode and on Windows via the native grow mode (ADR-0020): the engine reads the tree of the last session, grafts the source directory onto it and writes a new session that references the old file blocks in place. Appends a directory as a merged session; files from earlier sessions stay visible. |
| `optiburn probe` | Works on Linux (`/dev/sr*`) and Windows (drive letters). No drives found reports `未发现光驱` (exit 0). |
| Disc reading | Works. The read side (volume id, last-session check, tree listing, extraction) sits on the same `ReadBackend` seam with two backends: `xorriso` on Linux and a native MMC + ISO 9660/`UDF` reader on Windows (ADR-0018 and ADR-0021, verified on a real CD-R with both session-addressing conventions). |
| OptiBurn GUI | New in 0.1.1 (Tauri 2 + React). Four pages mirror the four subcommands; burn tasks can be cancelled mid-write. The device page browses a disc's file tree with drag and Ctrl/Shift multi-select and copies files straight to the system clipboard, the append page burns a list of files picked in the file dialog or pasted with Ctrl+V, it inherits the disc's current label, both disc pages can read the disc back and compare it with the source, and writes to a mounted disc are blocked with an unmount hint. Clipboard copy and the mount guard are Linux-only for now. Browsing, verification and the inherited label work on Windows through the native reader (ADR-0018); burning uses the native engine there too. Windows NSIS installers (x64, arm64) and Linux AppImage/deb packages (x86_64, arm64) ship from the releases page. AppImages embed update information, so AppImageUpdate works, and the GUI checks for updates in place on Windows and AppImage installs. |

Native MMC writing (no external tools) landed as `--engine native` on the CLI and is the GUI's burn engine on Windows, native disc reading (ADR-0018, ADR-0021) made the Windows GUI's browse, copy-staging and verification paths work without external tools, and the native grow mode (ADR-0020) makes `append` work there too; DVD/BD media are still untested. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the layered design and [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) for the media/filesystem matrix.

## Install

```bash
git clone https://github.com/ouyangjiahong26/optiburn
cd optiburn
cargo build --release
# binary: target/release/optiburn
```

- Rust 1.98.1 (pinned by `rust-toolchain.toml`).
- `xorriso` on `PATH` is needed by the CLI's default burn engine and by the GUI's Linux read and grow paths (browsing, verification, grow mode), and it is the reference reader in the tests. On Debian/Ubuntu: `sudo apt install xorriso`. On Windows there is no usable build: the MSYS2 package (`pacman -S xorriso`) is compiled without a drive-access backend, so it only works on image files, never on a drive (verified on hardware and against the upstream build logic on 2026-10-10; see the addendum in ADR-0008). Windows needs no external tool at all: the GUI and `--engine native` use the native MMC engine for burning and growing (ADR-0017, ADR-0020) and the native reader for browsing (ADR-0018, ADR-0021).
- Write access to the drive: usually membership in the `cdrom` group, or root.
- GUI: on Windows grab `OptiBurn_0.1.5_x64-setup.exe` (or the arm64 build). On Linux grab the AppImage (make it executable and run) or the `.deb`. Burning and reading use the native engine and native reader on Windows (no external tool); on Linux they use `xorriso` (see the note above). Offline Windows machines: grab the `-offline` build of the x64 installer instead; it embeds the WebView2 runtime (installer about 210 MB) and installs with zero downloads. On an unpatched Windows 7 the WebView2 installer fails with 0x8007007F: install KB2533623 and KB3063858 first, obtained offline from the [Microsoft Update Catalog](https://www.catalog.update.microsoft.com/Search.aspx?q=KB2533623) (see the addendum in ADR-0016).

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

Windows device names are drive letters: `--device E:`. By default the disc is left appendable (multi-session); pass `--close-disc` to finalize it. See [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) for the multi-session notes. `--engine` takes `xorriso` (the default; needs `xorriso` on `PATH`) or `native` (our own MMC commands, the only option on Windows); other values fail with an explicit "not implemented" error. The native engine writes a new session on an appendable disc and refuses finalized media, bad profiles, empty images and images that do not fit; it cannot set the write speed yet (leave `--speed` empty).

### Append to a disc

```bash
optiburn append photos --device /dev/sr0 --volume-id PHOTOS
```

`append` appends the directory contents at the disc root as a merged new session: files from earlier sessions stay visible, and Windows (which mounts the last session) sees all of them. On a blank disc this is equivalent to the first burn. `--close-disc` finalizes after committing; on randomly writable media (DVD-RAM, BD-RE) it has no effect, and xorriso keeps the disc rewritable. On Windows the native grow mode writes an ISO 9660 + Joliet session that references the old file blocks in place (ADR-0020); the drive keeps the disc appendable unless you pass `--close-disc`, and Explorer only refreshes its mount view after the disc is ejected and reinserted.

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
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/): the reference implementation this project tests against, and the 0.1.5 burn backend (invoked as a subprocess; no GPL code is linked).
- [alight](https://github.com/vicr123/alight): reference for how a Linux `SG_IO` transport is structured; no code copied (that repository carries no license).

## License

MIT, see [LICENSE](LICENSE).

