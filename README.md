# optiburn

![OptiBurn](assets/banner.png)

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

[English](README.md) | [简体中文](README.zh-CN.md)

optiburn is a cross-platform optical disc toolkit in Rust for Windows and Linux on x86_64 and arm64. It builds an image from a directory, writes that image or a new session to a drive, and reads the disc back. Images carry ISO 9660 and Joliet, plus UDF for the DVD and BD profiles (UDF 1.02 Bridge on DVD), so Windows and older readers can open them directly.

The project exists because software covering all four platform combinations is rare, and burning on Linux desktops (Ubuntu, Kylin and other distributions) is especially rough: command-line tools with cryptic flags, no progress, English-only errors, and disc states you have to diagnose by hand. optiburn puts pre-flight checks, live cancellable progress, classified failure messages and read-back verification around one core.

## What works today (0.1.11)

| Command | What it does | Platforms |
|---|---|---|
| `optiburn build-image` | Builds ISO 9660, Joliet and profile-appropriate UDF images. The byte layout is asserted in tests and compared with xorriso. | All four targets. |
| `optiburn burn` | Writes an image. Multi-session by default where the media allows. `--close-disc` finalizes. Native finalization covers CD and DVD-R only. | Linux: `xorriso` by default, `--engine native` optional. Windows: native only. |
| `optiburn append` | Reads the last session, grafts a directory onto it and writes a new session that references the old file blocks in place. | Native growth on both platforms. Linux falls back to xorriso growth on rewritable media, an unknown profile, or a requested speed. |
| `optiburn probe` | Lists drives, media state, capacity and the growth backend. Exits 0 when no drive is present. | Linux `/dev/sr*`, Windows drive letters. |
| `optiburn repair` | Closes the damaged track and session left by an interrupted burn so the disc can be written again. | Linux: xorriso `-close_damaged`. Windows: native. |
| `optiburn salvage` | Reads the unclosed track that standard readers cannot see, classifies every file against the written boundary and extracts the readable part. | Both platforms, through the native MMC reader. Run it before `repair`. |
| Disc reading | Volume id, last-session check, tree listing and extraction on one `ReadBackend` seam. | Linux: xorriso first, native retry when xorriso cannot read ISO 9660. Windows: native ISO 9660 and UDF. |
| GUI | Four function pages cover `probe`, `build-image`, `burn`, `append` and `salvage`; `repair` stays on the CLI, and a settings page holds the multi-device toggle (off by default). Burn jobs can be cancelled. | Tauri 2 + React. Windows uses the native burn, grow and read paths. Linux uses xorriso for burn and the first read path, and native growth for append. |

Hardware evidence, kept separate from the feature list:

- Native burning was verified on a CD-R on Windows on 2026-10-10 and on DVD-R on Linux on 2026-10-22 in two drives. DVD+R, DVD+RW and BD-R have no hardware write verification yet.
- Native growth was measured on Linux with a multi-session CD-R on 2026-10-15: preparation fell from 51.0 s to 1.6 s. DVD+R and DVD+RW append are still unverified. DVD-R and DVD-RW cannot take a second session.
- Disc reading was verified on a Windows CD-R on 2026-10-10 with both session-addressing conventions. On Linux the fallback to the native reader was verified on a device path on 2026-10-12, and a physical UDF-only disc is still pending.
- `salvage` was verified on an interrupted disc on 2026-10-11.

Every claim above is backed by an ADR or a test, and the code comments record the upstream quirks behind it. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains the seams, [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) the media matrix.

## Install

### Command line

Requirements:

- Rust 1.98.1, pinned by `rust-toolchain.toml`.
- `xorriso` on `PATH` for the default Linux burn engine, the first Linux read path, the xorriso growth fallback and the test reference. On Debian and Ubuntu: `sudo apt install xorriso`.
- Write access to the drive, usually membership in the `cdrom` group, or root.

```bash
git clone https://github.com/ouyangjiahong26/optiburn
cd optiburn
cargo build --release
# binary: target/release/optiburn
```

Windows needs no external tool: burning, growth and reading all use the native paths. There is no usable xorriso build for Windows either. The MSYS2 package is compiled without a drive-access backend, so it only works on image files, never on a drive (verified on hardware and against the upstream build logic on 2026-10-10, see the addendum in ADR-0008).

### GUI packages

Windows installers are `OptiBurn_0.1.11_x64-setup.exe` and the arm64 build. On Linux use the AppImage (make it executable and run) or the `.deb`. The `-offline` x64 installer embeds the WebView2 runtime, is about 210 MB and installs with zero downloads. On an unpatched Windows 7 the WebView2 installer fails with 0x8007007F, so install KB2533623 and KB3063858 first from the [Microsoft Update Catalog](https://www.catalog.update.microsoft.com/Search.aspx?q=KB2533623) (see the addendum in ADR-0016).

## Usage

### Build an image

```bash
$ optiburn build-image docs -o docs.iso --profile dvd --volume-id OPTIBURN
镜像: docs.iso
  扇区: 682
  字节: 1396736
  文件系统: ISO 9660, Joliet Level 3, UDF 1.02
```

The image embeds a build timestamp, so sizes track the directory contents. `--profile` picks the filesystems: `cd` (no UDF), `dvd` (default, UDF 1.02 Bridge) and `bd` (UDF 2.50). Without `-o` the image is written to `<source-directory-name>.iso` in the current directory.

### Burn it

```bash
optiburn burn docs.iso --device /dev/sr0 --speed 8
```

Windows device names are drive letters: `--device E:`. By default the disc is not finalized. Whether another session fits depends on the media. `--close-disc` asks the engine to finalize. The Linux xorriso default finalizes the media it supports, including DVD+R and BD-R. The native engine implements finalization for CD and DVD-R only, so DVD+R and BD-R need xorriso on Linux (ADR-0017). Before writing, the command waits for the media to become ready, refuses a finalized disc, refuses an appendable disc with an image because that would hide the old files and asks for `append` instead, allows rewritable media, and on Linux blocks a disc the system has mounted with an unmount hint.

`--engine` takes `xorriso` (the default, needs `xorriso` on `PATH`) or `native` (our own MMC commands, the only option on Windows). Other values are rejected. The native engine writes a new session on an appendable disc and refuses finalized media, bad profiles, empty images and images that do not fit. It cannot set the write speed yet, so leave `--speed` empty with `--engine native`.

### Append to a disc

```bash
optiburn append photos --device /dev/sr0 --volume-id PHOTOS
```

`append` maps the directory contents to the disc root as a merged new session. Files from earlier sessions stay visible, and Windows, which mounts the last session, sees all of them. On a blank disc this is the first burn. `--close-disc` finalizes after committing. The native engine supports CD and DVD-R, while DVD+R and BD-R need the xorriso backend on Linux.

On rewritable media (DVD-RAM, BD-RE) the native writer refuses to grow. Linux falls back to xorriso, where `--close-disc` has no effect, so the disc stays rewritable.

Native growth writes an ISO 9660 + Joliet session that references the old file blocks in place (ADR-0020). Explorer only refreshes its mount view after the disc is ejected and reinserted. If the old session has a shape the native writer cannot graft (no Joliet, a boot record, multi-extent files), the append is refused with the reason.

Linux falls back to xorriso growth on device facts only: rewritable media, an unknown profile, or a requested speed. The backend choice is not exposed in the interface; the `OPTIBURN_GROW_BACKEND` environment variable (`auto`, `native`, `xorriso`) is the diagnostic override for both the GUI and the CLI, and an explicit choice never falls back.

### Inspect a drive

```bash
$ optiburn probe
未发现光驱                      # no drive present (exit code 0)
# with a drive, one line per device:
# /dev/sr0 | ASUS BW-16D1HT 3.10 | 空盘，1 个区段
```

Exits 0 even when no drive is present (`未发现光驱`), so it is safe to run in scripts.

### Recover an interrupted burn

```bash
optiburn salvage --device D: --dest C:\rescued
optiburn repair --device D:
```

Run `salvage` first (`--device /dev/sr0` on Linux). It only reads the disc, and it can label every file as complete, truncated, or not written at all, then extract the readable part under the disc's own paths. A truncated `.zip` is rebuilt with only the entries that survived, so ordinary unzip tools can open it. No source file is needed: the written boundary and entry integrity are self-evident from the disc.

`repair` closes the damaged track and session the way libburn's `burn_disc_close_damaged` does, so more can be written. It removes the unclosed track that `salvage` reads, which is why the order matters. ADR-0022 records both paths.

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

## Learn more

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crate boundaries, data flow and the invariants at each seam.
- [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md): media and filesystem matrix, multi-session rules and verification steps.
- [docs/MULTI-DRIVE.md](docs/MULTI-DRIVE.md): writing to several drives at once, and the one-drive-one-entry rule.
- [docs-site](https://ouyangjiahong26.github.io/optiburn/): the user guide in Chinese.
- [CONTEXT.md](CONTEXT.md): the domain glossary used across code, tests and issues.

## Acknowledgements

- [hadris](https://github.com/hxyulin/hadris): the MIT-licensed Rust image writer (`hadris-cd`) behind the ISO 9660, Joliet and UDF layouts.
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/): the reference implementation this project tests against, and the default Linux burn backend (invoked as a subprocess, no GPL code is linked).
- [alight](https://github.com/vicr123/alight): reference for how a Linux `SG_IO` transport is structured. No code was copied, and that repository carries no license.

## License

MIT, see [LICENSE](LICENSE).
