# Changelog

本项目的所有显著变更都记录在本文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本管理遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

**中文**

### FEAT

- **原生读盘（ADR-0018）**：读侧五个能力（读卷标、末区段 ISO 门禁、列目录树、整树抽取、按路径抽取）接在新的 `ReadBackend` 接缝上：`XorrisoRead`（子进程，Linux 维持）与 `NativeRead`（MMC 读块加 hadris-iso 的 ISO 9660 解析，Windows 默认）。末区段定位用 READ TOC Format 1（libburn 同源），两种区段地址约定（区段相对与盘级绝对）按根目录首记录自引用自动探测。真机验证（Windows，USB 光驱，CD-R）：原生刻的末区段读卷标、列目录树、抽取并与镜像逐文件对拍一致，xorriso 增长的旧区段按绝对约定读通。离线与 xorriso 的列举对拍逐条一致。Windows 上设备页浏览、回读校验、卷标预填与 CLI `append` 的末区段门禁不再依赖外部工具。
- **原生增长模式（ADR-0020）**：`optiburn-engine` 新增嫁接式增长（`grow.rs` 与 `grow/old_session.rs`）：读出末区段的 Joliet 目录树，生成只含目录结构与新文件数据的新会话，旧文件的数据块原地引用，写进记录、路径表与描述符的地址一律盘级绝对，卷空间大小仍是区段相对值。Windows 上 `append` 与 GUI 追加页不再依赖 xorriso（Linux 维持 xorriso 增长模式），写序列与镜像刻录共用一条。形状不支持时显式报错并给中文文案：无 Joliet、含启动记录、多 extent 文件、目录成环或过深、读到的记录里带 SUSP 符号链接条目（SL），随机可写介质与已封口盘另走既有缺口文案。真机验证（Windows，USB 光驱，CD-R）：24 条目追加两个文件后读回 26 条，旧路径全部保留，新文件逐字节一致，同一会话由 Linux 内核 isofs（`sbsector=`）与 xorriso 独立复核一致。
- **UDF 盘读侧（ADR-0021）**：`disc_read/udf.rs` 用 `hadris-udf` 读出纯 UDF 盘与多区段 UDF 盘的末区段，三种定位约定（单段、区段相对、盘级绝对）按锚点的 tag_location 探测，盘级绝对约定下归一化锚点 tag 并重算校验和。打开顺序是 ISO 优先，Bridge 盘仍走 ISO 分支，追加门禁语义不变。读不了的结构（VAT、元数据分区）报明确文案。独立工具复核：`udfinfo` 与 Linux 内核 udf 驱动读同一批镜像，路径集合与文件 md5 与原生读侧逐个一致。
- **`optiburn-mmc` 增加会话信息命令**：READ TOC Format 1（`read_toc_session_info`）与 `DiscInformation::last_session_first_track`、`TrackInfo::track_blocks`，黄金 CDB 与解析测试按实测响应字节钉住。

- **原生 MMC 写引擎（ADR-0017）**：`optiburn-engine` 新增 `NativeEngine`，自己发 MMC 命令（MODE SELECT 写参数页、WRITE(10)、SYNCHRONIZE CACHE、CLOSE TRACK/SESSION，起点取 NWA），不依赖任何外部程序。`optiburn-mmc` 增加整套写侧命令与 READ(10)、READ TRACK INFORMATION（逐条黄金 CDB 测试）。CLI 新增 `--engine native`（默认仍是 xorriso），GUI 在 Windows 上默认走原生引擎。在 Windows 的 USB 光驱与一张 CD-R 上真机验证：写 81 块的新区段后用 READ(10) 读回，与镜像逐字节一致。可追加盘按 NWA 写新区段、不覆写已有内容。已封口盘、未知 Profile、空镜像与放不下的镜像给出明确文案拒绝。倍速参数尚未支持。DVD/BD 各族与增长模式留待后续。
- **真机验证落进代码与文档**：`inspect_real_media`（只读侦察）与 `native_burn_and_read_back_real`（写盘 + 读回对拍）两个 `--ignored` 真机测试。中文 README、文档站、平台表、ADR 同步更新。
- **盘片容量与写前门禁（ADR-0019）**：`optiburn-mmc` 新增 `read_disc_capacity` 与 `TrackInfo::free_blocks`。可用容量取 READ TRACK INFORMATION 的剩余块数（顺序介质上下一可写地址加剩余块数就是可写上限，实测与 xorriso 的整体容量一致），退回 READ FORMAT CAPACITIES 的格式化容量（描述符类型按字节 4 的低 2 位解码，类型 1 的最大可格式化容量整盘待写并参与门禁，类型 2 只作总容量展示）。追加的待写入量用 `xorriso -print_size` 预演，镜像刻录用文件大小，待写入加 16 MB 区段开销余量（`SESSION_OVERHEAD`）超过可用容量即拒绝，可用容量读不到时跳过并提示（CLI）。`probe`、CLI 的 `burn`/`append`、GUI 的刻录/追加路径与设备页、追加页都接入容量。

### FIX

- **缺 xorriso 的报错不再透出引擎内部英文串**：`MissingTool` 只带工具名，面向用户的说明改为中英各一份（中文收在引擎、CLI 与 GUI 共用，英文镜像在 GUI）。读盘路径（设备页浏览、复制、回读校验）此前没有映射这个错误，Windows 上会看到“读取盘片失败：missing tool: xorriso (sudo apt install xorriso)”，现在与刻录路径给同一份说明。
- **文档修正：Windows 上没有可用的 xorriso**：MSYS2 的包（`pacman -S xorriso`）编译时未链 libcdio，不含光驱访问，实测 `-devices` 报无 MMC 传输层、设备参数落进 libburn 的 stdio 伪设备。Windows 的读盘暂不可用（刻录已由同版本的原生引擎补上）。相关说法在 README、中文 README、文档站、ARCHITECTURE 的平台表同步，证据记在 ADR-0008 补记。

**English**

### FEAT

- **Native disc reading (ADR-0018)**: the five read capabilities (volume id, last-session ISO check, tree listing, whole-tree extraction, per-path extraction) move to a new `ReadBackend` seam with `XorrisoRead` (the subprocess, still used on Linux) and `NativeRead` (MMC block reads plus hadris-iso ISO 9660 parsing, the Windows default). The last session is located via READ TOC Format 1 (the same command libburn uses) and the two session-addressing conventions (session-relative and disc-absolute) are auto-detected from the root directory's self-referential first record. Verified on hardware (Windows, USB drive, CD-R): the natively burned last session reads back its volume id, tree and files matching the image byte for byte, and an xorriso-grown older session reads through the absolute convention; an offline listing comparison against xorriso matches entry for entry. Device-page browsing, verification, label prefill and the CLI append guard no longer need external tools on Windows.
- **Native grow mode (ADR-0020)**: `optiburn-engine` gains grafting growth (`grow.rs`, `grow/old_session.rs`): the engine reads the last session's Joliet tree and writes a new session holding only directory structures and the new file data, referencing the old file blocks in place. Addresses written into records, path tables and descriptors are disc-absolute, while the volume space size stays session-relative. `append` and the GUI append page no longer need xorriso on Windows (Linux keeps the xorriso grow mode) and the write sequence is shared with image burns. Unsupported shapes are refused with explicit text (no Joliet, boot record, multi-extent files, directory cycles or depth, SUSP symbolic-link entries (SL) in the records that are read), and rewritable or finalized media keep their existing gap messages. Verified on hardware (Windows, USB drive, CD-R): 24 entries plus two files read back as 26, old paths kept, new files byte-identical, and the same session read by the Linux kernel isofs (`sbsector=`) and xorriso.
- **UDF disc reading (ADR-0021)**: `disc_read/udf.rs` reads the last session of UDF-only and multi-session UDF discs through `hadris-udf`, probing three addressing conventions (single, session-relative, disc-absolute) from the anchor's tag_location and normalising the anchor tag together with its checksum in the disc-absolute case. Opening is ISO-first, so bridge discs keep the ISO branch and the append gate semantics are unchanged. Unsupported structures (VAT, metadata partition) get explicit text. Cross-checked with independent tools: `udfinfo` and the Linux kernel's UDF driver read the same images, with the same path set and file md5s as the native reader.
- **Session-information commands in `optiburn-mmc`**: READ TOC Format 1 (`read_toc_session_info`) plus `DiscInformation::last_session_first_track` and `TrackInfo::track_blocks`, with golden CDB and parse tests pinned to bytes captured from real hardware.

- **Native MMC burn engine (ADR-0017)**: `optiburn-engine` gains `NativeEngine`, which issues MMC commands itself (MODE SELECT write parameters, WRITE(10), SYNCHRONIZE CACHE, CLOSE TRACK/SESSION, starting at the NWA) with no external program; `optiburn-mmc` gains the whole write-side command set plus READ(10) and READ TRACK INFORMATION, each with golden CDB tests. The CLI accepts `--engine native` (xorriso stays the default) and the GUI uses the native engine on Windows. Verified on hardware (a USB drive and a CD-R on Windows): an 81-block session was written and read back with READ(10) byte for byte. Appendable discs are written as a new session at the NWA without overwriting existing data; finalized media, unknown profiles, empty images and oversized images are refused with explicit messages; write speed is not supported yet. DVD/BD media families and grow mode are future work.
- **Hardware verification landed in code and docs**: two `--ignored` tests, `inspect_real_media` (read-only recon) and `native_burn_and_read_back_real` (burn plus read-back compare); README, the platform table and the ADRs are updated.
- **Disc capacity and write gate (ADR-0019)**: `optiburn-mmc` gains `read_disc_capacity` and `TrackInfo::free_blocks`. Free capacity comes from READ TRACK INFORMATION's free blocks (on sequential media the next writable address plus the free blocks is the writable limit, verified against xorriso's overall capacity on hardware), falling back to READ FORMAT CAPACITIES (the descriptor type is decoded from the low two bits of byte 4: type 1 is the maximum formattable capacity of unformatted media and doubles as free space, type 2 is shown as total capacity only). Append sizes come from an `xorriso -print_size` rehearsal and image burns from the file size; a write whose size plus the 16 MB session overhead (`SESSION_OVERHEAD`) exceeds the free capacity is rejected, and when no free capacity is available the gate is skipped (the CLI prints a notice). `probe`, the CLI `burn`/`append` paths, the GUI burn/append tasks and the devices/append pages all surface the capacity.

### FIX

- **The missing-xorriso error no longer leaks engine internals**: `MissingTool` carries only the tool name, and user-facing guidance now lives in one shared Chinese copy (engine, used by CLI and GUI) plus an English mirror in the GUI. The read paths (device page browsing, copying, read-back verification) previously did not map this error, so Windows users saw "读取盘片失败：missing tool: xorriso (sudo apt install xorriso)"; they now get the same text as the burn path.
- **Docs: no usable xorriso on Windows**: the MSYS2 package (`pacman -S xorriso`) is built without libcdio and has no drive access; on hardware `-devices` reports no MMC transport and device arguments fall into libburn's stdio pseudo-drive. Disc reading on Windows is still unavailable, burning is covered by the native engine shipped in the same release. README, its Chinese mirror, the docs site and the ARCHITECTURE platform table are updated, and the evidence is recorded in the ADR-0008 addendum.

## [0.1.5] - 2026-10-10

**中文**

### FEAT

- **Windows 离线安装包**：Release 追加 `OptiBurn_<版本>_x64-setup-offline.exe`，内嵌 WebView2 Standalone（安装器约 210 MB），无网机器安装全程不需要下载任何东西。常规安装包维持在线引导器不变。Windows 7 上微软的安装器会自动装最后兼容的 WebView2 109。决策见 ADR-0016。

**English**

### FEAT

- **Windows offline installer**: the release now ships `OptiBurn_<version>_x64-setup-offline.exe` with the WebView2 Standalone runtime embedded (installer about 210 MB), so machines without internet install with zero downloads. The regular installer keeps its online bootstrapper. On Windows 7, Microsoft's installer automatically delivers the last compatible WebView2 109. See ADR-0016.

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/compare/v0.1.4...v0.1.5

## [0.1.4] - 2026-10-10

**中文**

图形前端补上更新能力：AppImage 嵌入更新信息并随 Release 发布 `.zsync`（AppImageUpdate 可增量更新，消除 AppImageHub 收录测试的 warning），应用内新增“检查更新”（Windows 安装包与 Linux AppImage 可就地更新，Ed25519 签名校验，deb 安装不显示入口）。CLI 无变化。

### FEAT

- **应用内检查更新**：侧栏底部新增入口，检查、确认、下载、安装闭环，更新源为 Release 的 `latest.json`，更新包经 Ed25519 签名校验。写盘任务进行中入口禁用。下载与安装分两步，安装只在没有任务进行时触发，进行中的刻录不会被安装动作打断。仅 Windows NSIS 与 Linux AppImage 显示（deb 安装的文件归包管理器管）。决策见 ADR-0015。
- **AppImage 嵌入式更新信息与 `.zsync`**：构建时经 `UPDATE_INFORMATION` 让 linuxdeploy 嵌入 `gh-releases-zsync` 更新信息并自动产出 `.zsync`，随 Release 发布，AppImageUpdate 用户从下个版本起可增量更新。

**English**

The GUI gains update capabilities: AppImages now embed update information and ship a `.zsync` alongside each release (AppImageUpdate users get delta updates; this also resolves the AppImageHub catalog warning), and an in-app "Check for updates" entry is added (in-place updates with Ed25519 signature verification for the Windows installer and the Linux AppImage; hidden for deb installs). No CLI changes.

### FEAT

- **In-app update checks**: a sidebar entry runs the check, confirm, download, install loop; the update source is the release's `latest.json` and packages are verified against an Ed25519 signature. The entry is disabled while a burn/append task is running; download and install are separate steps so the install only fires when no task is running and never interrupts a burn. Shown only for the Windows NSIS installer and the Linux AppImage (deb installs belong to the system package manager). See ADR-0015.
- **Embedded AppImage update information and `.zsync`**: the build passes `UPDATE_INFORMATION` through to linuxdeploy, embedding a `gh-releases-zsync` string and producing a `.zsync` published with the release, enabling AppImageUpdate delta updates from the next release on.

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/compare/v0.1.3...v0.1.4

## [0.1.3] - 2026-10-09

**中文**

图形前端按实机反馈完成一轮可用性改造：设备页可直接查看盘上文件并复制到系统剪贴板，追加页改为待刻录文件列表（文件多选、Ctrl+V 粘贴），卷标自动沿用盘上现有值，刻录与追加两页都能回读校验。CLI 与 GUI 的写前门禁新增挂载占用与末区段格式检查，写盘失败文案归类为中文说明。CLI 没有新增命令或选项。

### FEAT

- **界面跟随系统语言**：中文环境显示中文，其余默认英文。后端错误与任务消息同步双语，NSIS 安装器按系统语言选择界面语言。
- **设备页查看盘上文件并复制**：点击设备卡片展开末区段的目录树（名称与大小），支持拖动框选和 Ctrl、Shift 多选。“复制选中文件”把选中内容抽取到本地暂存目录后放进系统剪贴板，在文件管理器里可直接粘贴。决策见 ADR-0012。
- **追加页改为待刻录文件列表**：可用系统文件选择框多选文件，或在文件管理器复制后按 Ctrl+V（也提供“粘贴”按钮）加入列表，文件在盘根平铺写入。卷标自动沿用盘上现有值，读不到时保持默认。决策见 ADR-0011。
- **刻录与追加页新增回读校验**：把盘上最后一区段抽到本地，与源（追加的待刻录文件或刻录的镜像）逐文件对比内容，差异清单随界面语言（中英）。决策见 ADR-0010。
- **写前门禁新增两类检查（CLI 与 GUI 同源）**：光盘被系统挂载时拒绝写入并给出卸载指引。可追加盘的末区段不是 ISO 9660（例如 Windows 的 UDF 盘）时拒绝续写，避免原有文件被遮蔽。决策见 ADR-0010。
- **写盘失败文案按成因归类**：光驱断连、设备被占用等已知成因按界面语言输出说明（含对盘的影响与下一步），原始输出写到 stderr 备查。决策见 ADR-0013。

### FIX

- **追加页 Ctrl+V 无响应**：改为拦截按键并由后端读取系统剪贴板（WebKit 的 paste 事件在焦点不在可编辑控件上时不派发）。
- **切换页面丢失设备页状态**：四个页面改为常驻挂载，盘上清单、选择状态与进行中的复制在切换页面后保留。
- **卷标预填可能带入假值**：盘上没有可读的 ISO 9660 时不再预填 xorriso 空镜像的默认卷标。卷标读取改用 shared 打开，盘被挂载时也能预填。
- **盘上浏览与复制的加固**：符号链接条目按链接自身路径解析。抽取前校验盘内路径，拒绝盘符前缀与上级目录。末区段不是 ISO 9660 的盘在设备页按错误提示，不再显示成空盘。复制改占任务槽，可中止、与写盘互斥、退出确认会等它收尾。
- **写前门禁提前到暂存之前**：挂载、封口、末区段格式的拒绝不再先白拷一遍文件。暂存失败同样清理临时目录，混进列表的目录得到中文说明。
- **校验失败文案区分来源**：校验读取失败不再套用“刻录失败”字样，盘上内容与镜像分别给出说明。
- **追加页粘贴限定在本页**：Ctrl+V 只在追加页可见且没有任务时生效，不再从别的页面静默改动待刻录列表。
- **切页与设备状态的修正**：设备、追加与刻录页恢复在页面可见时刷新探测。盘上清单的迟到响应与陈旧选中锚点不再串设备。
- **GUI 发行二进制必须经 tauri CLI 构建**：裸 `cargo build --release` 产出开发态二进制，窗口会去连开发服务器并报连接被拒。开发命令说明已更新。

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/compare/v0.1.2...v0.1.3

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
