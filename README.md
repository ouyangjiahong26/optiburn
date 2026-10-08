# optiburn

[![CI](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml/badge.svg)](https://github.com/ouyangjiahong26/optiburn/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/github/license/ouyangjiahong26/optiburn)](https://github.com/ouyangjiahong26/optiburn/blob/main/LICENSE)

[English](README.md) | [简体中文](README.zh-CN.md)

Cross-platform optical disc burning toolkit in Rust for Linux and Windows on x86_64 and arm64. It builds disc images that Windows can read (ISO 9660 + Joliet + UDF Bridge) and writes them to a drive.

## What it does today (0.1.0)

| Command | State |
|---|---|
| `optiburn build-image` | Works: builds ISO 9660 + Joliet + UDF Bridge images, verified against xorriso in tests. |
| `optiburn burn` | Works on Linux. Multi-session by default (the disc stays appendable); `--close-disc` finalizes. |
| `optiburn append` | Works on Linux. Appends a directory as a merged session; files from earlier sessions stay visible. |
| `optiburn probe` | Works on Linux (`/dev/sr*`). Other platforms find no devices and report `未发现光驱` (exit 0). |

Native MMC writing (no external tools) is the next milestone, not part of 0.1.0. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the layered design and [docs/WINDOWS-COMPAT.md](docs/WINDOWS-COMPAT.md) for the media/filesystem matrix.

## Install

```bash
git clone https://github.com/ouyangjiahong26/optiburn
cd optiburn
cargo build --release
# binary: target/release/optiburn
```

- Rust 1.98.1 (pinned by `rust-toolchain.toml`).
- `xorriso` on `PATH`, only for `burn`, and only because 0.1.0 shells out to it: `sudo apt install xorriso` on Debian/Ubuntu, `pacman -S xorriso` on MSYS2.
- Write access to the drive: usually membership in the `cdrom` group, or root.

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
- [xorriso / libburnia](https://www.gnu.org/software/xorriso/): the reference implementation this project tests against, and the 0.1.0 burn backend (invoked as a subprocess; no GPL code is linked).
- [alight](https://github.com/vicr123/alight): reference for how a Linux `SG_IO` transport is structured; no code copied (that repository carries no license).

## License

MIT, see [LICENSE](LICENSE).

