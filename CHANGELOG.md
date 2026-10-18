# Changelog

本项目的所有显著变更都记录在本文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本管理遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

**中文**

### 特性

- 读盘抽取改为逐项报告：整树抽取与按路径抽取遇到个别文件读不出时，返回完整文件与读不出文件的清单，不再把部分失败压成整棵失败。GUI 的离盘复制保留完整文件并列出读不出的路径，回读校验把读不出的文件与内容差异分开报。xorriso 后端只按落盘结果报告完整文件，显式选择它时拿不到逐项状态（ADR-0029）。

### 修复

- Linux 上侧栏左下角的版本提示不再消失：版本行此前与“检查更新”入口共用一个开关，安装形态不能就地更新时（deb 安装、开发态运行）整个区域返回 null，版本号被一并隐藏。现在版本提示单列成一个恒定显示的组件，版本号取自应用自身的版本，Windows 与 AppImage 的表现不变。

**English**

### FEAT

- Disc extraction now returns a per-file report: when one or more files cannot be read, whole-tree and by-path extraction return the complete files plus the unreadable paths instead of failing as a whole. The GUI copy keeps the complete files and lists the unreadable paths, and read-back verification reports unreadable files separately from content differences. The xorriso backend only reports files it landed, so an explicit xorriso selection still lacks per-file status (ADR-0029).

### FIX

- The version label in the bottom-left corner no longer disappears on Linux: it shared one switch with the Check for updates entry, so installations that cannot update in place (deb packages, development runs) returned null for the whole area and hid the version together with it. The label is now a separate component that always renders, taking the version from the application itself; Windows and AppImage look unchanged.

## [0.1.8] - 2026-10-16

**中文**

### FEAT

- **一次向多台光驱写入（ADR-0025）**：任务槽从全局单槽改成按设备键控，同一设备互斥、不同设备并行。刻录页与追加页改成设备与内容配对表，点“添加设备”逐行配设备与内容，提交按行顺序逐行发起，某一台被拒（占用或门禁失败）只把错误留在那一行，已发起的行照常写入。内容区顶部的“当前任务”清单逐台显示进度与停止按钮，可单独停某一台，一台失败或被停不影响其它台。被任务占用的光驱在下拉里标注“任务进行中”并禁用，该行的输入与校验按钮一并禁用。设备页的复制与抢救按展开的设备判断占用，该设备有任务在跑时禁用。CLI 不变，同一台设备不要同时被 GUI 与 CLI 选中（操作口径见 `docs/MULTI-DRIVE.md`）。

### 修复

- **写前容量门禁不再用一个 16 MB 常数（ADR-0019 修订）**：区段开销按动作与介质取值。保持可追加的追加优先用盘上实测几何（末区段的下一可写地址与它的数据量之差，本机 CD-R 上 7335 块约 15 MB），读不到时用 CD 默认值（首区段 11845 块、非首区段 7335 块）并取标准下限兜底。封盘不再按大数拦（首区段 6750 块、后续区段 2250 块，ECMA-394 的导出区下限），同一张盘上可用 11547 块（22.6 MB）时写下过 4536 块的封盘区段，按旧口径会被误拒。拒绝文案把待写入、区段开销、可用容量三个数字摊开，不再出现“待写入 8.3 MB、可用 22.6 MB”这种看不出原因的消息。DVD 与 BD 仍沿用 16 MB 兜底（没有本机实测），封盘那次另按 ECMA-349 的导出区下限 5120 扇区兜底，实测依据见 ADR-0019 修订。
- 增长模式（追加）新增保守开关 `OPTIBURN_GROW_BACKEND`（`auto` 默认、`xorriso`、`native`）：Linux 上取 `native` 可把追加的写前准备（末区段门禁判定、尺寸预演）与写入整条切到原生引擎，写前窗口里不再起子进程。写前门禁的末区段判定改为跟随写盘引擎，`probe` 每台设备多报一行增长模式后端。默认档在这一条落地时仍是 xorriso，同一发布内随后切到原生，见下面“变更”一节。真机验证（2026-10-15，多区段 CD-R，HL-DT-ST GP70N）：默认档写前 51.0 秒、总计 81.4 秒，原生档写前 0.3 秒、总计 16.9 秒，两档写入的文件都读回一致，原生增长在 Linux 上的写序列由此验证通过一次，嫁接数据的逐字节校验也通过（220 MB 的安装包 sha256 与 Release 产物一致，ADR-0028）。
- 新增写前阶段计时诊断 `OPTIBURN_TIMING`：置位后 CLI 与 GUI 命令层每段完成往 stderr 打一行（`阶段 尺寸预演：4.2 秒`，GUI 多一段暂存拷贝，从终端启动可见）。默认档的 Linux 追加准备会依次起三个 xorriso 子进程（末区段查询、尺寸预演、提交），每次都要重新初始化驱动器并读进整份盘上 ISO 结构，计时行就是用来在真机上分辨这十几秒花在哪一段（ADR-0028）。

- 修正非 UTF-8 环境下追加会写坏盘上原有非 ASCII 文件名的问题：会碰文件名的 xorriso 调用（增长模式的尺寸预演与提交、卷标、列目录树、整树抽取、按路径抽取）一律固定 `-local_charset UTF-8`，名字侧字符集与消息侧解析用的 `LC_ALL=C` 解耦。此前在 `LC_ALL=C` 一类环境里跑一次追加，libisofs 会把旧区段每个非 ASCII 名换成带哈希的替代名（`____hcm____._t_x_t…`）写进新会话，一次写介质上不可回修。真机验证那张多区段 CD-R 的第 3 个区段（本次改动之前写入）就是这种形态。回归测试 `grow_keeps_joliet_names_under_a_c_locale` 去掉该参数即失败（ADR-0026）。

### 变更

- 增长模式（追加）的默认后端切到原生：Linux 与 Windows 的 `auto` 都走原生增长，写前窗口不再起子进程（真机 2026-10-15，同一张多区段 CD-R：写前 51.0 秒降到 1.6 秒）。Linux 上按设备事实回退 xorriso 增长模式，判据只有三条：随机可写介质（盘片状态位 0b11）、驱动器报的 Profile 不在支持名单、指定倍速。显式选择优先于默认且不回退。镜像刻录仍按平台分派（ADR-0027）。
- 追加页新增“增长模式后端”选择（跟随默认 / 原生增长 / xorriso 增长模式），回落发生时结果横幅里说明原因。CLI 在同一步打一行说明。`probe` 的增长模式后端行改为报实际默认与回退条件。
- 已知边界写进平台表与 README：DVD±R 与 BD-R 的原生写序列未经真机验证。增长区段不带 Rock Ridge（xorriso 增长带）。写盘质量未测。

**English**

### FEAT

- **Write to several optical drives at once (ADR-0025)**: job slots are now keyed by device instead of one global slot, so one drive at a time stays exclusive while different drives run in parallel. The burn and append pages became a device-to-content pairing table: click Add device to add a row, each row picks one drive and one image or file list, and submitting walks the rows in order. A row that is refused (busy drive, gate failure) keeps its error in that row and does not stop the rows already started. The Current tasks list on top shows per-drive progress and a Stop button for each task, so one drive can be stopped without touching the others. A drive that already has a task running is marked busy in the dropdown and disabled, together with that row's inputs and verify button. On the devices page, copying and salvaging are disabled for the expanded drive while a task runs on it. The CLI is unchanged; do not drive one drive from the GUI and the CLI at the same time (see `docs/MULTI-DRIVE.md`).

### FIX

- **The write gate no longer uses a flat 16 MB session overhead (ADR-0019 revision)**: the overhead is now per action and per media. Appends that keep the disc appendable prefer the disc's own geometry (the next writable address minus the last session's data size, 7335 blocks or about 15 MB on the measured CD-R), fall back to a CD default (11845 blocks for the first session, 7335 for later ones) and never go below the spec floor. Finalizing writes are no longer held to the large figure (6750 blocks for the first session, 2250 for later ones, ECMA-394's lead-out minimum); a 4536-block finalizing session was written on a disc with 11547 blocks (22.6 MB) free, which the old rule refused. The refusal text now shows three numbers (to write, session overhead, free), so it no longer reads like "8.3 MB to write, 22.6 MB free" with no explanation. DVD and BD keep the 16 MB fallback (no hardware measurement yet), with ECMA-349's 5120-sector lead-out minimum as a floor for finalizing writes; the evidence is recorded in the ADR-0019 revision.
- The growth mode (append) gains the conservative switch `OPTIBURN_GROW_BACKEND` (`auto` by default, `xorriso`, `native`): on Linux, `native` moves the whole write preparation (last-session gate check, size rehearsal) and the write itself to the native engine, so the pre-write window spawns no subprocess. The pre-write gate's last-session check now follows the writing engine, and `probe` prints one more line for the growth backend per device. The default mode was still xorriso when this landed and flips to native later in the same release, see CHANGED below. Verified on hardware (2026-10-15, multi-session CD-R, HL-DT-ST GP70N): the default mode spends 51.0 s before writing and 81.4 s in total, the native mode 0.3 s and 16.9 s, and the files written by both modes read back identical, which verifies the native growth write sequence on Linux once, and the byte-for-byte check of the grafted old files passed as well (the 220 MB installer's sha256 matches the release artifact, both archives pass `unzip -t`, ADR-0028).
- New stage-timing diagnostic `OPTIBURN_TIMING`: with it set, the CLI and the GUI command layer print one line per stage to stderr (`阶段 尺寸预演：4.2 秒`, the GUI has an extra staging copy, visible when launched from a terminal). The default Linux append preparation runs three xorriso subprocesses in sequence (last-session query, size rehearsal, commit), each re-initializing the drive and loading the whole ISO structure from the disc, and the timing lines are how those tens of seconds get attributed on real hardware (ADR-0028).
- Append no longer rewrites existing non-ASCII file names when the environment is not UTF-8: every xorriso call that touches file names (the grow size rehearsal and commit, the volume id, tree listing, whole-tree extraction, per-path extraction) now pins `-local_charset UTF-8`, decoupling the name charset from the `LC_ALL=C` used to parse messages. Previously a single append under a C locale made libisofs replace every non-ASCII name of the old session with a hashed substitute (`____hcm____._t_x_t…`) and write it into the new session, which cannot be repaired on write-once media; session 3 of the multi-session CD-R used for hardware verification (written before this change) has exactly that shape. The regression test `grow_keeps_joliet_names_under_a_c_locale` fails without the flag (ADR-0026).

### CHANGED

- The default backend of the growth mode (append) is now the native engine: `auto` runs native growth on Linux and Windows alike, so the pre-write window spawns no subprocess (hardware, 2026-10-15, same multi-session CD-R: preparation dropped from 51.0 s to 1.6 s). On Linux it falls back to xorriso grow mode on device facts only: random-writable media (disc status 0b11), a profile outside the supported list, or a requested write speed. An explicit choice wins over the default and never falls back. Image burning still dispatches by platform (ADR-0027).
- The append page gained a growth-backend choice (follow default / native growth / xorriso grow mode) and the result banner now explains a fallback; the CLI prints one line instead. The `probe` growth-backend line now reports the default and its fallback conditions.
- Known boundaries are recorded in the platform table and README: the native write sequence for DVD±R and BD-R is unverified on hardware; native-grown sessions carry no Rock Ridge (xorriso-grown ones do); burn quality is not measured.

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/compare/v0.1.7...v0.1.8

## [0.1.7] - 2026-10-12

**中文**

### 修复

- Windows 追加时为重复的 ISO 9660 短名分配编号，保留 Joliet 中文文件名。盘上已有的长中文文档名转换后相同，也能继续追加其他文件，旧文件的数据块仍原地引用。

**English**

### FIX

- **Duplicate ISO 9660 short names are numbered on Windows appends**: appends now number duplicate ISO 9660 short names and keep the Joliet Chinese names. Long Chinese document names already on the disc that convert to the same short name no longer block appending further files, and the old files' data blocks are still referenced in place.

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/compare/v0.1.6...v0.1.7

## [0.1.6] - 2026-10-12

**中文**

### FEAT

- **Linux 读侧的后端回退（ADR-0024）**：读盘五个能力（卷标、末区段格式门禁、目录树列举、整树抽取、按路径抽取）与抢救在 Linux 改为 xorriso 优先，xorriso 读不到 ISO 9660（输出报兜底空镜像或读侧报错）时后端回退到原生读侧重试同一次操作。xorriso 1.5.6 只解析 ISO 9660，纯 UDF 盘（Windows 系统自带刻录的默认输出）在它眼里是空盘，此前读不了，现在这类盘在 Linux 上卷标、列举、抽取都可用，抢救未关闭轨道也随之可用。保守开关 `OPTIBURN_READ_BACKEND`（`auto` 默认、`xorriso` 退回改动前行为、`native` 只走原生）可退回，`probe` 每台设备多报一行读侧后端现状。损坏末区段的候选回退（ADR-0022）在 Linux 顺带可用。真空白盘与缺 xorriso 不触发后端回退，分别报“盘是空盘”与安装提示。合成盘夹具已全绿，真机证据由维护者后补并记回 issue #41。
- **Windows 上也能把盘上文件复制到剪贴板（ADR-0012 补记）**：设备页的“复制选中文件”在 Windows 上用 CF_HDROP 文件列表加复制语义放进剪贴板，文件管理器里可直接粘贴。追加页的 Ctrl+V 读同一格式。原先这条路上的“复制到系统剪贴板尚未支持该平台”不再出现。剪贴板只放路径，暂存文件在应用存活期间保留，与 Linux 侧同一套约定。
- **未关闭轨道的抢救（ADR-0022 补记）**：刻录中断留下的未关闭轨道不在 TOC 里，标准读取器（含 xorriso）看不见它，现在 CLI 的 `optiburn salvage --device D: --dest DIR` 能把里面的数据拿出来：列出它的目录树、按写入边界给每条文件定性（完整、半截、一个字节都没写）、把能读的部分按盘上路径抽出来。GUI 设备页在盘可追加或随机可写时也给出同一入口（一行按钮，说明放在确认框里，选好目标目录后与写盘同占任务槽，结果随完成消息给出），两处走同一个引擎函数，界面呈现约定见 ADR-0023。写入边界靠驱动器定位（二分扫描，读得出来的块就是写下去了，实测这张盘紧邻中断点报 `MEDIUM ERROR`、再往后报 `LBA out of range`），半截的 `.zip` 按本地头链与描述符自洽性只保留完整条目（存储条目再核 CRC32，压缩条目只核长度）、重建中央目录与 EOCD，重建后常规解压工具能直接用。中断点落在第一条里面、或条目数总长超出普通 zip 的表达范围时按原始字节保留并把原因报出来。不需要源文件：边界与条目完整性都由盘上结构自证，别的电脑刻坏、盘拿到本机也能救。Linux 的 xorriso 读侧没有这个能力，返回明确文案。
- **原生读盘（ADR-0018）**：读侧五个能力（读卷标、末区段 ISO 门禁、列目录树、整树抽取、按路径抽取）接在新的 `ReadBackend` 接缝上：`XorrisoRead`（子进程，Linux 维持）与 `NativeRead`（MMC 读块加 hadris-iso 的 ISO 9660 解析，Windows 默认）。末区段定位用 READ TOC Format 1（libburn 同源），两种区段地址约定（区段相对与盘级绝对）按根目录首记录自引用自动探测。真机验证（Windows，USB 光驱，CD-R）：原生刻的末区段读卷标、列目录树、抽取并与镜像逐文件对拍一致，xorriso 增长的旧区段按绝对约定读通。离线与 xorriso 的列举对拍逐条一致。Windows 上设备页浏览、回读校验、卷标预填与 CLI `append` 的末区段门禁不再依赖外部工具。
- **原生增长模式（ADR-0020）**：`optiburn-engine` 新增嫁接式增长（`grow.rs` 与 `grow/old_session.rs`）：读出末区段的 Joliet 目录树，生成只含目录结构与新文件数据的新会话，旧文件的数据块原地引用，写进记录、路径表与描述符的地址一律盘级绝对，卷空间大小仍是区段相对值。Windows 上 `append` 与 GUI 追加页不再依赖 xorriso（Linux 维持 xorriso 增长模式），写序列与镜像刻录共用一条。形状不支持时显式报错并给中文文案：无 Joliet、含启动记录、多 extent 文件、目录成环或过深、读到的记录里带 SUSP 符号链接条目（SL），随机可写介质与已封口盘另走既有缺口文案。真机验证（Windows，USB 光驱，CD-R）：24 条目追加两个文件后读回 26 条，旧路径全部保留，新文件逐字节一致，同一会话由 Linux 内核 isofs（`sbsector=`）与 xorriso 独立复核一致。
- **损坏末区段的回退与续写（ADR-0022）**：刻录中断留下坏末区段的盘（固件把残片登记成区段）现在能救回来。读盘按 `READ TOC` 的轨道起点加末区段起点枚举候选，从新到旧逐个验证（卷描述符区加整棵目录树），用最新的可用区段列举与抽取，追加把嫁接源换到这个区段、仍写在 NWA 之后，但要求显式确认（CLI 的 `--allow-damaged-last-session`，GUI 追加页的勾选项），因为被跳过区段里的文件不再出现在盘上的可见目录里。驱动器层面的损坏按 libburn 的口径判定：`READ TRACK INFORMATION` 的 Damage 位与 NWA_V 位决定盘还能不能写。报损坏时先按 libburn 的 `burn_disc_close_damaged` 顺序尝试关闭损坏的轨道与区段（新增 `optiburn repair --device <设备> [--force]`，等价于 xorriso 的 `-close_damaged as_needed|force`），修复不成功就按 "Damaged, not closed and not writable" 拒绝并给出读数据换盘的指引。Windows 侧生效，Linux 侧交给 xorriso 的同名能力。
- **UDF 盘读侧（ADR-0021）**：`disc_read/udf.rs` 用 `hadris-udf` 读出纯 UDF 盘与多区段 UDF 盘的末区段，三种定位约定（单段、区段相对、盘级绝对）按锚点的 tag_location 探测，盘级绝对约定下归一化锚点 tag 并重算校验和。打开顺序是 ISO 优先，Bridge 盘仍走 ISO 分支，追加门禁语义不变。读不了的结构（VAT、元数据分区）报明确文案。独立工具复核：`udfinfo` 与 Linux 内核 udf 驱动读同一批镜像，路径集合与文件 md5 与原生读侧逐个一致。
- **`optiburn-mmc` 增加会话信息命令**：READ TOC Format 1（`read_toc_session_info`）与 `DiscInformation::last_session_first_track`、`TrackInfo::track_blocks`，黄金 CDB 与解析测试按实测响应字节钉住。

- **原生 MMC 写引擎（ADR-0017）**：`optiburn-engine` 新增 `NativeEngine`，自己发 MMC 命令（MODE SELECT 写参数页、WRITE(10)、SYNCHRONIZE CACHE、CLOSE TRACK/SESSION，起点取 NWA），不依赖任何外部程序。`optiburn-mmc` 增加整套写侧命令与 READ(10)、READ TRACK INFORMATION（逐条黄金 CDB 测试）。CLI 新增 `--engine native`（默认仍是 xorriso），GUI 在 Windows 上默认走原生引擎。在 Windows 的 USB 光驱与一张 CD-R 上真机验证：写 81 块的新区段后用 READ(10) 读回，与镜像逐字节一致。可追加盘按 NWA 写新区段、不覆写已有内容。已封口盘、未知 Profile、空镜像与放不下的镜像给出明确文案拒绝。倍速参数尚未支持。DVD/BD 各族与增长模式留待后续。
- **真机验证落进代码与文档**：`inspect_real_media`（只读侦察）与 `native_burn_and_read_back_real`（写盘 + 读回对拍）两个 `--ignored` 真机测试。中文 README、文档站、平台表、ADR 同步更新。
- **盘片容量与写前门禁（ADR-0019）**：`optiburn-mmc` 新增 `read_disc_capacity` 与 `TrackInfo::free_blocks`。可用容量取 READ TRACK INFORMATION 的剩余块数（顺序介质上下一可写地址加剩余块数就是可写上限，实测与 xorriso 的整体容量一致），退回 READ FORMAT CAPACITIES 的格式化容量（描述符类型按字节 4 的低 2 位解码，类型 1 的最大可格式化容量整盘待写并参与门禁，类型 2 只作总容量展示）。追加的待写入量用 `xorriso -print_size` 预演，镜像刻录用文件大小，待写入加 16 MB 区段开销余量（`SESSION_OVERHEAD`）超过可用容量即拒绝，可用容量读不到时跳过并提示（CLI）。`probe`、CLI 的 `burn`/`append`、GUI 的刻录/追加路径与设备页、追加页都接入容量。

### FIX

- **Linux 上原生读侧认不出设备路径**：`/dev/sr0` 此前被判成镜像文件（判定只认 Windows 盘符），而块设备的读取长度是 0，原生读侧没读一个字节就报“读不到 ISO 9660”。设备路径上的原生读取、UDF 读取与抢救因此一直不生效，ADR-0024 的后端回退也打不开设备。现按 `/dev/` 前缀认作光驱。实测（多区段 CD-R）：改动前卷标、列举、抽取、抢救在原生侧全部失败，改动后卷标 `RECOVERY`、列举 27 条、抽取 25 个文件、抢救未关闭轨道 26 个文件，抽出的清单与内核挂载同一张盘逐条一致。
- **挂载中的盘整树抽取失败**：xorriso 的 `-extract` 参数表少了 `-drive_access shared`，盘被桌面环境自动挂载时独占打开报 `Cannot open busy device`（实测）。与其它只读参数表同一口径后可抽。
- **别的写入器写的盘不再被目录大小拒掉**：旧区段（增长模式的嫁接源、抢救的抢救源）读取原先把目录记录的 data_len 按“必须是块大小的整数倍”硬校验，而 libisofs 一系的写入器（xorriso 等）写的是实际字节数。实测一张中断会话的根目录是 426 与 572 字节，这类盘此前直接读不出来（抢救与追加都会失败），现在按块向上取整读、按实际长度遍历记录。
- **缺 xorriso 的报错不再透出引擎内部英文串**：`MissingTool` 只带工具名，面向用户的说明改为中英各一份（中文收在引擎、CLI 与 GUI 共用，英文镜像在 GUI）。读盘路径（设备页浏览、复制、回读校验）此前没有映射这个错误，Windows 上会看到“读取盘片失败：missing tool: xorriso (sudo apt install xorriso)”，现在与刻录路径给同一份说明。
- **文档修正：Windows 上没有可用的 xorriso**：MSYS2 的包（`pacman -S xorriso`）编译时未链 libcdio，不含光驱访问，实测 `-devices` 报无 MMC 传输层、设备参数落进 libburn 的 stdio 伪设备。Windows 的读盘暂不可用（刻录已由同版本的原生引擎补上）。相关说法在 README、中文 README、文档站、ARCHITECTURE 的平台表同步，证据记在 ADR-0008 补记。

**English**

### FEAT

- **Read-side backend fallback on Linux (ADR-0024)**: the five read capabilities (volume id, last-session format gate, tree listing, whole-tree extraction, per-path extraction) and salvaging now try xorriso first on Linux and fall back to the native read side, retrying the same operation, when xorriso cannot find ISO 9660 (the blank-image fallback message or a read-side error). xorriso 1.5.6 only parses ISO 9660, so a UDF-only disc (the default output of Windows' built-in burning) looked like a blank disc and could not be read; on Linux its volume id, listing and extraction now work, and salvaging an unclosed track works there too. The conservative switch `OPTIBURN_READ_BACKEND` (`auto` default, `xorriso` to restore the old behaviour, `native` for the native side only) reverts it, and `probe` prints one more line per device showing the read backend. The damaged-last-session candidate fallback (ADR-0022) becomes available on Linux as a side effect. A truly blank disc and a missing xorriso do not trigger the fallback; they report "the disc is blank" and the install hint. The synthetic-disc fixtures are green; on-hardware evidence will be added by the maintainer and recorded on issue #41.
- **Copying disc files to the clipboard now works on Windows (ADR-0012 addendum)**: the device page's "copy selected files" puts a CF_HDROP file list with copy semantics on the clipboard, ready to paste in Explorer, and the append page's Ctrl+V reads the same format. The previous "not supported on this platform" error is gone on this path. Like the Linux side, the clipboard carries paths only and the staged files are kept until the app exits.
- **Salvaging an unclosed track (ADR-0022 addendum)**: an interrupted burn leaves an unclosed track that is not in the TOC, so standard readers (xorriso included) cannot see it. `optiburn salvage --device D: --dest DIR` now gets the data out: it lists the track's own directory tree, classifies every file against the written boundary (complete, truncated, not written at all) and extracts the readable part under the disc's paths. The boundary comes from the drive (binary search; a block that reads is a block that was written, and on the measured disc reads just past the cut report `MEDIUM ERROR` and then `LBA out of range`), and a truncated `.zip` keeps only the entries whose local header chain is self-consistent (stored entries are additionally CRC32-checked, compressed ones length-checked), rebuilding the central directory and EOCD so ordinary unzip tools open it. No source file is needed: boundary and entry integrity are self-evident from the disc, so a disc burned on another machine can be salvaged here. The GUI device page offers the same entry as one line with the explanation in a confirm dialog (ADR-0023). The xorriso read path on Linux has no such capability and says so.
- **Native disc reading (ADR-0018)**: the five read capabilities (volume id, last-session ISO check, tree listing, whole-tree extraction, per-path extraction) move to a new `ReadBackend` seam with `XorrisoRead` (the subprocess, still used on Linux) and `NativeRead` (MMC block reads plus hadris-iso ISO 9660 parsing, the Windows default). The last session is located via READ TOC Format 1 (the same command libburn uses) and the two session-addressing conventions (session-relative and disc-absolute) are auto-detected from the root directory's self-referential first record. Verified on hardware (Windows, USB drive, CD-R): the natively burned last session reads back its volume id, tree and files matching the image byte for byte, and an xorriso-grown older session reads through the absolute convention; an offline listing comparison against xorriso matches entry for entry. Device-page browsing, verification, label prefill and the CLI append guard no longer need external tools on Windows.
- **Native grow mode (ADR-0020)**: `optiburn-engine` gains grafting growth (`grow.rs`, `grow/old_session.rs`): the engine reads the last session's Joliet tree and writes a new session holding only directory structures and the new file data, referencing the old file blocks in place. Addresses written into records, path tables and descriptors are disc-absolute, while the volume space size stays session-relative. `append` and the GUI append page no longer need xorriso on Windows (Linux keeps the xorriso grow mode) and the write sequence is shared with image burns. Unsupported shapes are refused with explicit text (no Joliet, boot record, multi-extent files, directory cycles or depth, SUSP symbolic-link entries (SL) in the records that are read), and rewritable or finalized media keep their existing gap messages. Verified on hardware (Windows, USB drive, CD-R): 24 entries plus two files read back as 26, old paths kept, new files byte-identical, and the same session read by the Linux kernel isofs (`sbsector=`) and xorriso.
- **Damaged last session recovery (ADR-0022)**: discs left with a broken last session by an interrupted burn (firmware registers the fragment as a session) can now be salvaged. Reading enumerates candidates from READ TOC track starts plus the last session start and validates them newest first (descriptor area plus the whole directory tree), then lists and extracts from the newest usable session. Appending uses that session as the graft source and still writes after the NWA, but requires explicit confirmation (CLI `--allow-damaged-last-session`, the GUI append page checkbox) because files in the skipped sessions no longer appear in the disc's visible directory. Drive-level damage is classified the way libburn does it, from the Damage and NWA_V bits of READ TRACK INFORMATION; when the drive reports damage the engine first attempts libburn's `burn_disc_close_damaged` sequence to close the damaged track/session (new `optiburn repair --device <device> [--force]`, equivalent to xorriso's `-close_damaged as_needed|force`) and refuses with "Damaged, not closed and not writable" plus read-back advice when that fails. Windows only; on Linux this is delegated to the same xorriso option.
- **UDF disc reading (ADR-0021)**: `disc_read/udf.rs` reads the last session of UDF-only and multi-session UDF discs through `hadris-udf`, probing three addressing conventions (single, session-relative, disc-absolute) from the anchor's tag_location and normalising the anchor tag together with its checksum in the disc-absolute case. Opening is ISO-first, so bridge discs keep the ISO branch and the append gate semantics are unchanged. Unsupported structures (VAT, metadata partition) get explicit text. Cross-checked with independent tools: `udfinfo` and the Linux kernel's UDF driver read the same images, with the same path set and file md5s as the native reader.
- **Session-information commands in `optiburn-mmc`**: READ TOC Format 1 (`read_toc_session_info`) plus `DiscInformation::last_session_first_track` and `TrackInfo::track_blocks`, with golden CDB and parse tests pinned to bytes captured from real hardware.

- **Native MMC burn engine (ADR-0017)**: `optiburn-engine` gains `NativeEngine`, which issues MMC commands itself (MODE SELECT write parameters, WRITE(10), SYNCHRONIZE CACHE, CLOSE TRACK/SESSION, starting at the NWA) with no external program; `optiburn-mmc` gains the whole write-side command set plus READ(10) and READ TRACK INFORMATION, each with golden CDB tests. The CLI accepts `--engine native` (xorriso stays the default) and the GUI uses the native engine on Windows. Verified on hardware (a USB drive and a CD-R on Windows): an 81-block session was written and read back with READ(10) byte for byte. Appendable discs are written as a new session at the NWA without overwriting existing data; finalized media, unknown profiles, empty images and oversized images are refused with explicit messages; write speed is not supported yet. DVD/BD media families and grow mode are future work.
- **Hardware verification landed in code and docs**: two `--ignored` tests, `inspect_real_media` (read-only recon) and `native_burn_and_read_back_real` (burn plus read-back compare); README, the platform table and the ADRs are updated.
- **Disc capacity and write gate (ADR-0019)**: `optiburn-mmc` gains `read_disc_capacity` and `TrackInfo::free_blocks`. Free capacity comes from READ TRACK INFORMATION's free blocks (on sequential media the next writable address plus the free blocks is the writable limit, verified against xorriso's overall capacity on hardware), falling back to READ FORMAT CAPACITIES (the descriptor type is decoded from the low two bits of byte 4: type 1 is the maximum formattable capacity of unformatted media and doubles as free space, type 2 is shown as total capacity only). Append sizes come from an `xorriso -print_size` rehearsal and image burns from the file size; a write whose size plus the 16 MB session overhead (`SESSION_OVERHEAD`) exceeds the free capacity is rejected, and when no free capacity is available the gate is skipped (the CLI prints a notice). `probe`, the CLI `burn`/`append` paths, the GUI burn/append tasks and the devices/append pages all surface the capacity.

### FIX

- **The native read side did not recognise device paths on Linux**: `/dev/sr0` was classified as an image file (the check only knew Windows drive letters) and a block device reports a read length of 0, so the native side answered "no ISO 9660 image" without reading a byte. Native reading, UDF reading and salvaging therefore never worked on a device path, and ADR-0024's backend fallback could not open a drive either. Paths under `/dev/` are now recognised as drives. Measured on a multi-session CD-R: before the fix volume id, listing, extraction and salvage all failed on the native side; after it the volume id is `RECOVERY`, the listing has 27 entries, extraction yields 25 files and salvaging the unclosed track yields 26 files, matching the kernel's mount of the same disc entry for entry.
- **Whole-tree extraction failed on a mounted disc**: the xorriso `-extract` argument table lacked `-drive_access shared`, so an auto-mounted disc was refused with `Cannot open busy device` (measured). It now matches the other read-only argument tables.
- **Discs written by other writers no longer fail on directory sizes**: reading an older session (the grow graft source and the salvage source) used to require the directory record's data_len to be a multiple of the block size, while libisofs-family writers (xorriso and friends) store the exact byte count. A measured interrupted session had root directories of 426 and 572 bytes, and such discs were unreadable before (both salvage and append failed). The reader now rounds the read up to blocks and walks records up to the exact length.
- **The missing-xorriso error no longer leaks engine internals**: `MissingTool` carries only the tool name, and user-facing guidance now lives in one shared Chinese copy (engine, used by CLI and GUI) plus an English mirror in the GUI. The read paths (device page browsing, copying, read-back verification) previously did not map this error, so Windows users saw "读取盘片失败：missing tool: xorriso (sudo apt install xorriso)"; they now get the same text as the burn path.
- **Docs: no usable xorriso on Windows**: the MSYS2 package (`pacman -S xorriso`) is built without libcdio and has no drive access; on hardware `-devices` reports no MMC transport and device arguments fall into libburn's stdio pseudo-drive. Disc reading on Windows is still unavailable, burning is covered by the native engine shipped in the same release. README, its Chinese mirror, the docs site and the ARCHITECTURE platform table are updated, and the evidence is recorded in the ADR-0008 addendum.

**Full Changelog**: https://github.com/ouyangjiahong26/optiburn/compare/v0.1.5...v0.1.6

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
