# Changelog

本项目的所有显著变更都记录在本文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本管理遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

**中文**

图形前端按实机反馈完成一轮可用性改造：设备页可直接查看盘上文件并复制到系统剪贴板，追加页改为待刻录文件列表（文件多选、Ctrl+V 粘贴），卷标自动沿用盘上现有值，刻录与追加两页都能回读校验。CLI 与 GUI 的写前门禁新增挂载占用与末区段格式检查，写盘失败文案归类为中文说明。CLI 没有新增命令或选项。

### FEAT

- **设备页查看盘上文件并复制**：点击设备卡片展开末区段的目录树（名称与大小），支持拖动框选和 Ctrl、Shift 多选。“复制选中文件”把选中内容抽取到本地暂存目录后放进系统剪贴板，在文件管理器里可直接粘贴。决策见 ADR-0012。
- **追加页改为待刻录文件列表**：可用系统文件选择框多选文件，或在文件管理器复制后按 Ctrl+V（也提供“粘贴”按钮）加入列表，文件在盘根平铺写入。卷标自动沿用盘上现有值，读不到时保持默认。决策见 ADR-0011。
- **刻录与追加页新增回读校验**：把盘上最后一区段抽到本地，与源（追加的待刻录文件或刻录的镜像）逐文件对比内容，差异列成中文清单。决策见 ADR-0010。
- **写前门禁新增两类检查（CLI 与 GUI 同源）**：光盘被系统挂载时拒绝写入并给出卸载指引。可追加盘的末区段不是 ISO 9660（例如 Windows 的 UDF 盘）时拒绝续写，避免原有文件被遮蔽。决策见 ADR-0010。
- **写盘失败文案按成因归类**：光驱断连、设备被占用等已知成因输出中文说明（含对盘的影响与下一步），原始输出写到 stderr 备查。决策见 ADR-0013。

### FIX

- **追加页 Ctrl+V 无响应**：改为拦截按键并由后端读取系统剪贴板（WebKit 的 paste 事件在焦点不在可编辑控件上时不派发）。
- **切换页面丢失设备页状态**：四个页面改为常驻挂载，盘上清单、选择状态与进行中的复制在切换页面后保留。
- **卷标预填可能带入假值**：盘上没有可读的 ISO 9660 时不再预填 xorriso 空镜像的默认卷标；卷标读取改用 shared 打开，盘被挂载时也能预填。
- **盘上浏览与复制的加固**：符号链接条目按链接自身路径解析；抽取前校验盘内路径，拒绝盘符前缀与上级目录；末区段不是 ISO 9660 的盘在设备页按错误提示，不再显示成空盘；复制改占任务槽，可中止、与写盘互斥、退出确认会等它收尾。
- **写前门禁提前到暂存之前**：挂载、封口、末区段格式的拒绝不再先白拷一遍文件；暂存失败同样清理临时目录，混进列表的目录得到中文说明。
- **校验失败文案区分来源**：校验读取失败不再套用「刻录失败」字样，盘上内容与镜像分别给出说明。
- **追加页粘贴限定在本页**：Ctrl+V 只在追加页可见且没有任务时生效，不再从别的页面静默改动待刻录列表。
- **切页与设备状态的修正**：设备、追加与刻录页恢复在页面可见时刷新探测；盘上清单的迟到响应与陈旧选中锚点不再串设备。
- **GUI 发行二进制必须经 tauri CLI 构建**：裸 `cargo build --release` 产出开发态二进制，窗口会去连开发服务器并报连接被拒。开发命令说明已更新。

## [0.1.2] - 2026-10-08

**中文**

本版把图形前端带到 Linux：x86_64 与 aarch64 各发 AppImage（单文件免安装）与 deb 安装包，四个页面对应四个子命令，刻录类任务可中止，刻录仍要求 xorriso 在 PATH 上。Windows 侧维持 NSIS 安装包不变。CLI 对外行为不变，本版无破坏性变更。

### FEAT

- **Linux 图形前端安装包（AppImage 与 deb）**：`release.yml` 新增 `build-gui-linux` job，
  x86_64 在 `ubuntu-latest`、aarch64 在公开仓库免费的 `ubuntu-24.04-arm` 原生 runner 上
  构建，各产出 AppImage 与 deb。GUI 代码本身零平台分支（CI 的 gui job 已在 ubuntu 上
  编译并测试），刻录仍要求 PATH 上的 xorriso，安装包不内嵌外部工具。决策见 ADR-0009。(PR #18)

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/compare/v0.1.1...v0.1.2

## [0.1.1] - 2026-10-08

**中文**

本版新增图形前端（Tauri 2 + React，Windows 发 NSIS 安装包），四个页面对应四个子命令，刻录类任务可中止，任务未结束时关窗需要确认。核心 crate 同步增强：刻录前门禁下沉 `optiburn-mmc`，`optiburn-engine` 新增协作式取消，`optiburn-transport` 补齐 Windows 光驱枚举。CLI 对外行为不变，本版无破坏性变更。

### FEAT

- **图形前端（Tauri 2 + React）**：新增 `src-tauri/`（Tauri 2 命令层，包 `optiburn-gui`）与
  `frontend/`（React 19 + Vite + TS），与 CLI 平级覆盖四个子命令（`probe`、`build-image`、
  `burn`、`append`），对应设备、制作镜像、刻录、追加四个页面。任务运行中锁定界面并显示
  进度，刻录类任务可中止，任务未结束时关窗需要确认。
  Windows 发 NSIS 安装包（x64 与 arm64）。核心 crate 同步增强：`optiburn-mmc` 下沉就绪
  等待与写入门禁（`wait_until_ready`、`approve_write`），`optiburn-engine` 新增协作式
  取消令牌（`CancelToken`、`BurnError::Cancelled`），`optiburn-transport` 补齐 Windows
  光驱枚举。决策见 ADR-0008。(PR #15)

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/compare/v0.1.0...v0.1.1

## [0.1.0] - 2026-10-08

**中文**

本版发布首个可用版本，覆盖四个方面：Windows 可读的镜像生成（ISO 9660 + Joliet + UDF Bridge）、xorriso 子进程刻录引擎、默认多区段与增长模式追加、只读 probe。镜像层经 xorriso 回读逐字节对拍验证，刻录与追加在真实光驱上实测（空白 CD-R 先刻后追加，回读逐字节一致）。本版无破坏性变更，这是第一个发布版本。

### FEAT

- **5-crate Cargo workspace 骨架**：`optiburn-transport`（SG_IO/SPTI SCSI 传输）、`optiburn-mmc`（MMC 读侧命令）、`optiburn-mastering`（hadris-cd ISO9660/Joliet/UDF 镜像生成）、`optiburn-engine`（xorriso 子进程刻录引擎）、`optiburn-cli`。(PR #1)
- **`optiburn append <目录>` 增长模式追加刻录**：xorriso 增长模式（`-dev … -map <目录> / -commit`）读出盘上已有区段的目录树，新区段同时携带新旧文件，旧文件保持可见。空盘时直接写第一区段，等价于首刻。(PR #10)
- **刻录前置检查自动路由介质状态**：`burn` 与 `append` 共用前置检查，先等介质就绪（20 秒内每 500 ms 重试），再按盘片状态路由：空盘放行，已封口拒绝，可追加盘拒绝镜像路径并指引 `append`（独立镜像会遮住已有区段的文件），随机可写介质（DVD-RAM、BD-RE）放行。(PR #10)
- **`burn` 默认多区段不封盘**：默认保持盘可追加，`--close-disc` 显式封盘。`append` 的同名参数在提交前加 `-close on`，对随机可写介质不生效（xorriso 手册明示）。(PR #10)

### FIX

- **Linux 打开光驱设备节点一律带 `O_NONBLOCK`**：空白盘上 `O_RDWR` 阻塞打开被内核判 `EROFS`，随盘片识别时序间歇复现。怪癖记录见 ADR-0007。(PR #10)
- **CLI 顶层帮助文案改为中文**：与本仓库“面向用户的文案用中文”的约定一致。(PR #3)
- **`ImageInfo.sectors` 只在字节数是 2048 整数倍时给出**：否则返回新错误 `MisalignedImage`，不再用向上取整推算一个无法核实的扇区数。(PR #3)
- **镜像路径按 `OsStr` 原样传给 xorriso**：非 UTF-8 路径不再被替换成 U+FFFD，并有回归测试钉住。(PR #3)
- **平台相关修正**：Windows 目标不再产生未使用导入与死代码警告（cross-check 现在以 `-D warnings` 跑）。其它平台的传输层测试不再假定 `open()` 必然返回 `NotFound`。(PR #3)
- **CLI 中文文案的冒号与括号改为全角**：“错误：”“打开失败：”“其它（3）”等。(#5, PR #9)

### CLEANUP

- **probe 的设备枚举移到 `optiburn_transport::list_optical_devices`**：与设备打开并列按平台分发，CLI 删除全部 `#[cfg(target_os)]`。非 Linux 平台从报“probe 尚未支持该平台”改为报“未发现光驱”，退出码同为 0。(#6, PR #9)

### DOC

- **软件设计文档**：架构分层、Windows 兼容性矩阵、7 份 ADR（0006 默认多区段与增长模式、0007 Linux 打开光驱必须 O_NONBLOCK）、术语表（`docs/`）。(PR #1, PR #10)
- **治理**：CI（lint/test/cross-check）、发布工作流、分支保护、安全报告、议题与 PR 模板。(PR #1)
- **文档纠正三处不实声明**：镜像“字节跨次一致”（实际含构建时刻时间戳）、引擎区分 `unsupported`、以及 `READ DISC INFORMATION` 字节 3 的语义（盘上首轨号）。(PR #3)
- **文档与注释统一为弯引号，术语改用 `CONTEXT.md` 术语表词汇**（盘片状态、区段、设备路径、UDF Bridge）。(PR #3)
- **新增中文 README（`README.zh-CN.md`）**：英文 README 同步四个子命令与默认行为。(PR #11)

完整决策依据见 `docs/adr/` 对应 ADR，工程术语见 `CONTEXT.md`。

---

**English**

This release ships the first usable version across four areas: Windows-readable image mastering (ISO 9660 + Joliet + UDF Bridge), the xorriso subprocess burn engine, multi-session by default with grow-mode appends, and a read-only probe. The image layer is verified by a byte-for-byte xorriso round trip, and burning plus appending were exercised on a real drive (blank CD-R, burn then append, byte-for-byte match on read-back). No breaking changes; this is the first release.

### FEAT

- **5-crate Cargo workspace skeleton**: `optiburn-transport` (SG_IO/SPTI SCSI transport), `optiburn-mmc` (MMC read-side commands), `optiburn-mastering` (ISO 9660/Joliet/UDF image building via hadris-cd), `optiburn-engine` (xorriso subprocess burn engine), `optiburn-cli`. (PR #1)
- **`optiburn append <directory>` grow-mode appending**: xorriso grow mode (`-dev … -map <directory> / -commit`) reads the existing session tree from the disc and writes a new session carrying both old and new files, so files from earlier sessions stay visible. On a blank disc it writes the first session, equivalent to a first burn. (PR #10)
- **Pre-burn media checks shared by `burn` and `append`**: wait for the medium to become ready (retry every 500 ms for up to 20 s), then route on disc state: blank discs proceed, finalized discs are rejected, appendable discs reject the image path with a pointer to `append` (a standalone image would shadow earlier sessions), randomly writable media (DVD-RAM, BD-RE) proceed. (PR #10)
- **`burn` is multi-session by default**: the disc stays appendable unless `--close-disc` is passed. The `append` counterpart adds `-close on` before committing, which has no effect on randomly writable media (as the xorriso manual states). (PR #10)

### FIX

- **Optical device nodes on Linux always open with `O_NONBLOCK`**: on blank discs a blocking `O_RDWR` open can be judged `EROFS` by the kernel, depending on disc-detection timing. The quirk is recorded in ADR-0007. (PR #10)
- **CLI top-level help text switched to Chinese**: consistent with the repo rule that user-facing text is Chinese. (PR #3)
- **`ImageInfo.sectors` only when the byte count is a multiple of 2048**: otherwise a new `MisalignedImage` error is returned instead of rounding up an unverifiable sector count. (PR #3)
- **Image paths are passed to xorriso as raw `OsStr`**: non-UTF-8 paths are no longer replaced with U+FFFD; a regression test pins this. (PR #3)
- **Platform-related corrections**: Windows targets no longer produce unused-import and dead-code warnings (cross-check now runs with `-D warnings`); transport-layer tests on other platforms no longer assume `open()` returns `NotFound`. (PR #3)
- **CLI Chinese text now uses full-width colons and brackets**: “错误：”“打开失败：”“其它（3）” and others. (#5, PR #9)

### CLEANUP

- **probe device enumeration moved to `optiburn_transport::list_optical_devices`**: dispatched per platform next to `open`, removing every `#[cfg(target_os)]` from the CLI. On non-Linux platforms probe changed from “probe 尚未支持该平台” to “未发现光驱”, exit code 0 in both cases. (#6, PR #9)

### DOC

- **Design documentation**: layered architecture, Windows compatibility matrix, 7 ADRs (0006 multi-session by default and grow mode, 0007 `O_NONBLOCK` when opening optical devices), glossary (`docs/`). (PR #1, PR #10)
- **Governance**: CI (lint/test/cross-check), release workflow, branch protection, security reporting, issue and PR templates. (PR #1)
- **Three false documentation claims corrected**: “bytes identical across runs” (the image embeds a build timestamp), the engine having an `unsupported` branch, and the meaning of `READ DISC INFORMATION` byte 3 (first track on disc). (PR #3)
- **Curly quotes and `CONTEXT.md` terminology unified across docs and comments** (disc state, session, device path, UDF Bridge). (PR #3)
- **Chinese README added (`README.zh-CN.md`)**: the English README gained the four subcommands and the new default behavior. (PR #11)

Full rationale lives in the ADRs under `docs/adr/`; terminology in `CONTEXT.md`.

---

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/commits/v0.1.0
