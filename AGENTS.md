# AGENTS.md

optiburn：Rust 写的跨平台光盘刻录工具（Linux/Windows × x86_64/arm64）。
把目录做成 Windows 能读的镜像（ISO 9660 + Joliet + UDF Bridge），再把镜像写到光驱。

## Project Overview

五个 crate 的 workspace，依赖单向：`cli` 依赖 `mastering`、`engine`、`mmc`，并直接依赖
`transport`（probe 用它打开与枚举设备）。`mastering` 依赖 `hadris-cd`，`mmc` 依赖
`transport`。

- `optiburn-transport`：SCSI 传输。全仓库唯一的硬件抽象点，`ScsiTransport` trait 只有
  一个方法 `issue(cdb, dir, data, timeout)`。Linux 走 `SG_IO`，Windows 走 SPTI，光驱设备
  枚举（`list_optical_devices`）也按平台收在这里。CDB 上限 16 字节，
  sense 上限 32 字节，命令级成功 = SCSI 状态字节与宿主机/驱动状态全 0。
- `optiburn-mmc`：MMC 命令编解码。v0 只有读侧三条（`inquiry`、`test_unit_ready`、
  `read_disc_information`）。写侧（`RESERVE TRACK`/`WRITE(10)`/`CLOSE TRACK`）在路线图上，
  落地前不留空壳类型。
- `optiburn-mastering`：`build_image(source_dir, output, spec) -> ImageInfo`。profile 到文件
  系统的映射（`options_for`）是 Windows 可读性的唯一落点。输出文件必须以读写方式打开
  （hadris 写完卷描述符会回读打补丁，只写句柄会 `EBADF`）。
- `optiburn-engine`：`BurnEngine` trait + `XorrisoEngine`（`xorriso -as cdrecord` 子进程）。
  原生 MMC 引擎将接在同一个 trait 上。
- `optiburn-cli`：`optiburn build-image | burn | append | probe`。二进制名是 `optiburn`
  （`[[bin]] name`），不是 `optiburn-cli`。`burn` 默认多区段不封盘，追加刻录走
  `append`（xorriso 增长模式，ADR-0006）。

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
| `crates/optiburn-mastering/` | `build_image` + profile 映射。`tests/roundtrip.rs` 是 xorriso 对拍 |
| `crates/optiburn-engine/` | `lib.rs`（trait/`BurnJob`）、`xorriso.rs`（参数与进度解析） |
| `crates/optiburn-cli/` | `src/main.rs` 三个子命令 |
| `docs/` | `ARCHITECTURE.md`、`WINDOWS-COMPAT.md`、`adr/`（0001–0005） |
| `CONTEXT.md` | 领域术语表（术语/定义/禁用同义词三列），命名一律照它 |
| `.github/workflows/` | `ci.yml`（job 名即分支保护的 required checks）、`release.yml` |

## 约定

- 改接口先过全量检查：任何公开符号变化都要 `cargo check --workspace --all-targets`
  加四个目标全过，再跑测试。
- 新公开符号配错误类型：错误用 `thiserror`，`#[error]` 文案英文。CLI 面向用户的
  文案中文。
- 注释写中文，只写“为什么”。文件 < 1000 行、函数 < 50 行。
- 不加 GUI 依赖，不引 GPL 依赖（链接层面必须保持 MIT 可分发，子进程调 GPL 工具可以）。
- 平台分支只能出现在 `optiburn-transport`。别处出现 `#[cfg(target_os)]` 前先想清楚。
- 上游怪癖要写进代码注释与 ADR（例：hadris 需要读写句柄，它的文档注释里 UDF VRS 的
  扇区号是错的，实测在 20–22）。这类事实写下来，避免下一个人重踩。
- 不写空洞的兼容层：改接口就迁移全部调用方。废弃代码直接删。
- 领域命名用 `CONTEXT.md` 词汇。`main` 走分支保护，一切改动经 PR。
- 能用证据（命令输出、字节偏移、上游源码位置）就不用推测。推测要标明是推测。
- 文档只写中文，`README.md` 例外（英文）。

## 写作要求

所有面向人读的文本（注释、CONTEXT.md、ADR、issue 评论、PR 描述、agent brief、分诊记录、文档、Agent 回复）应当：

- 准确、清楚、简洁。先理解材料，再提炼结论。
- 按逻辑组织，区分相近概念。不用空泛、夸大的修饰语。
- 面向实际读者，从已知事实推到陌生结论。用分析说服，不装腔或堆砌。
- 语言统一中文，不中英混用。命令、代码、路径、专有名词保留原文。
- 概念不直译。没有通行译名的概念用功能描述，不生造译名。同一概念全文只用一个词：用“标准”不用“规范”，用“任务要求”不用“规格”，测试结果用“通过、失败”不用“红、绿”。
- 标点符号：中文语境用全角中文标点（句号“。”、逗号“，”、顿号“、”）。纯英文句子用英文标点。行内代码、文件名、命令后的标点根据所在句子语境选择。引号：中文用弯引号“”，英文用直引号""。列举用顿号（“、”）分隔，最后一项前不加“和”或“与”。
- 不用破折号（“——”、“—”）、分号、箭头（“→”）、markdown 加粗和表情符号。补充说明用括号，并列与分支写成独立句子，顺序与对应关系用文字表达。
- 单位用国标：带单位的数值用 GB 3100～3102 的法定计量单位。写单位符号时，数值与符号之间留一个空格（`200 ms`、`5 min`、`64 MB`），不写 `200MS`、`64MB` 这类变体。中文行文里用汉字单位名称（30 秒、5 分钟）同样合规。

## 编码准则

- 先理解再改动：完整阅读目标文件、相似实现和相关测试。不确定 API 或惯例时查源码或文档，不猜。
- 明确目标与决策：需求或验收条件不明确时先澄清。架构选择、假设和关键取舍要说明。
- 保持简单：只实现当前需求。复用已有模式。不为单一用例过早抽象、配置化或引入依赖。
- 精准修改：只改与任务直接相关的代码，贴合既有风格。删掉本次修改产生的废弃代码，不重格式化无关内容。
- 注释不留历史：删掉只对读过旧版本的人有意义的注释（“原来是 X”“改成 Y 是为了修 Z”）。注释解释当前代码为什么这样写，变更过程由 git 记录。
- 完整迁移：变更接口或行为时更新所有调用方、测试和文档。不保留无需求的兼容层。
- 按根因修复：先复现并读完整错误信息。一次处理一个原因，不用吞异常或特判掩盖问题。
- 验证行为：按影响范围运行相关检查。测试可观察行为、边界和错误路径，不测试实现细节。无法测试时说明原因并做可行的烟雾验证。
- 审慎依赖：优先现有依赖和标准库。新增依赖前确认必要性、维护状态和成本，并说明理由。
- 清楚沟通：说明做了什么、为什么、验证结果和已知风险。对不确定性给出具体事实，提交信息描述实际改动。
