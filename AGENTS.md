# AGENTS.md

optiburn：Rust 写的跨平台光盘刻录工具（Linux/Windows × x86_64/arm64）。
把目录做成 Windows 能读的镜像（ISO 9660 + Joliet + UDF Bridge），再把镜像写到光驱。

## Project Overview

五个 crate 的 workspace，依赖单向：`cli → {mastering, engine, mmc} → {hadris-cd, transport}`。

- `optiburn-transport`：SCSI 传输。全仓库唯一的硬件抽象点——`ScsiTransport` trait 一个方法
  `issue(cdb, dir, data, timeout)`；Linux 走 `SG_IO`，Windows 走 SPTI。CDB 上限 16 字节，
  sense 上限 32 字节，命令级成功 = SCSI 状态字节与宿主机/驱动状态全 0。
- `optiburn-mmc`：MMC 命令编解码。v0 只有读侧三条（`inquiry`、`test_unit_ready`、
  `read_disc_information`）。写侧（`RESERVE TRACK`/`WRITE(10)`/`CLOSE TRACK`）在路线图上，
  落地前不留空壳类型。
- `optiburn-mastering`：`build_image(source_dir, output, spec) -> ImageInfo`。profile 到文件
  系统的映射（`options_for`）是 Windows 可读性的唯一落点。输出文件必须以读写方式打开
  （hadris 写完卷描述符会回读打补丁，只写句柄会 `EBADF`）。
- `optiburn-engine`：`BurnEngine` trait + `XorrisoEngine`（`xorriso -as cdrecord` 子进程）。
  原生 MMC 引擎将接在同一个 trait 上。
- `optiburn-cli`：`optiburn build-image | burn | probe`。二进制名是 `optiburn`
  （`[[bin]] name`），不是 `optiburn-cli`。

## Development Commands

```bash
cargo build --workspace
cargo test --workspace                 # 需要 xorriso（镜像回读对拍会真跑）
cargo fmt --all && cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings

# 四个目标三元组（Windows 路径只做 check：交叉编译不链接）
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
         x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
  cargo check --workspace --all-targets --target "$t"
done

# 端到端（无光驱可跑的部分）
cargo run -p optiburn-cli -- build-image docs -o /tmp/e2e.iso --profile dvd --volume-id E2E
xorriso -indev /tmp/e2e.iso -ls /
cargo run -p optiburn-cli -- probe      # 期望“未发现光驱”，退出码 0
```

硬件相关测试：`cargo test -p optiburn-engine -- --ignored burn_real` 需要
`OPTIBURN_DEVICE` 与 `OPTIBURN_IMAGE`，本机与 CI 都没有光驱，默认不跑。

## Key Directories

| 路径 | 用途 |
|---|---|
| `crates/optiburn-transport/` | `lib.rs`（trait + 平台分发）、`linux.rs`（SG_IO）、`windows.rs`（SPTI） |
| `crates/optiburn-mmc/` | CDB 编解码与响应解析，含黄金 CDB 断言与固定缓冲区解析测试 |
| `crates/optiburn-mastering/` | `build_image` + profile 映射；`tests/roundtrip.rs` 是 xorriso 对拍 |
| `crates/optiburn-engine/` | `lib.rs`（trait/`BurnJob`）、`xorriso.rs`（参数与进度解析） |
| `crates/optiburn-cli/` | `src/main.rs` 三个子命令 |
| `docs/` | `ARCHITECTURE.md`、`WINDOWS-COMPAT.md`、`adr/`（0001–0005） |
| `CONTEXT.md` | 领域术语表（术语/定义/禁用同义词三列），命名一律照它 |
| `.github/workflows/` | `ci.yml`（job 名即分支保护的 required checks）、`release.yml` |

## 约定

- **改接口先过全量检查**：任何公开符号变化都要 `cargo check --workspace --all-targets`
  加四个目标全过，再跑测试。
- **新公开符号配错误类型**：错误用 `thiserror`，`#[error]` 文案英文；CLI 面向用户的
  文案中文。
- **注释写中文**，只写“为什么”。文件 < 1000 行、函数 < 50 行。
- **不加 GUI 依赖**，不引 GPL 依赖（链接层面必须保持 MIT 可分发；子进程调 GPL 工具可以）。
- **平台分支只能出现在 `optiburn-transport`**。别处出现 `#[cfg(target_os)]` 前先想清楚。
- **上游怪癖要写进代码注释与 ADR**（例：hadris 需要读写句柄；它的文档注释里 UDF VRS 的
  扇区号是错的，实测在 20–22）。这类事实写下来，避免下一个人重踩。
- **不写空洞的兼容层**：改接口就迁移全部调用方；废弃代码直接删。
- 领域命名用 `CONTEXT.md` 词汇；`main` 走分支保护，一切改动经 PR。

## 写作要求

面向人读的文本（注释、文档、ADR、issue/PR 正文、CHANGELOG）要求：

- 准确、简洁，先给结论再说依据；不堆形容词，不写营销腔。
- 区分相近概念，不用空泛说法；术语照 `CONTEXT.md`。
- 能用证据（命令输出、字节偏移、上游源码位置）就不用推测；推测要标明是推测。
- 全仓库文档与注释用弯引号（“”），不用直角引号（「」）；中英文混排时数字与英文两侧不加空格以外的东西。
- 文档只写中文，`README.md` 例外（英文为主 + “中文速览”一节）。
