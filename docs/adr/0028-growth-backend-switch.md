# ADR-0028：Linux 追加的写前准备开销与增长模式开关

- 状态：已接受
- 日期：2026-10-15

## 背景

维护者报告：同一张多区段盘上的追加（GUI 的追加页），Windows 点下去就开始写，Linux
要先准备十几秒才开始写数据。要弄清这段时间花在哪里。

代码侧的进程账（`src-tauri/src/cmd/mod.rs` 的 `check_write_gates` 与 `run_disc_task`，
CLI 的 `ensure_burnable` 与 `append_command`）是这样的：Linux 上一次追加的准备窗口里
会起三个 xorriso 子进程。

1. 门禁的末区段查询：`-drive_access shared -indev <设备> -pvd_info`
   （`readback.rs` 的 `pvd_info_args`）。
2. 尺寸预演：`-dev <设备> -map <目录> / -print_size`（`xorriso.rs` 的 `print_size_args`）。
3. 提交：同参数表换 `-commit`（`grow_args`）。它自己的写前阶段还要先读盘上旧区段。

Windows 在同一条路径上一个子进程都没有：门禁的末区段判定、尺寸预演与写入分别走
原生读侧、原生会话计划与原生引擎（ADR-0018、ADR-0020），全在本进程内发 MMC 命令。
两边盘上要做的事（读旧区段目录树、生成新区段、写）是一样的，差的是每次 xorriso
调用都要重新把驱动器初始化一遍，而这些初始化在子进程退出时全部作废。

每次 xorriso 调用的固定开销有两部分，都能从上游源码与实测对上：

- 驱动器初始化。libburn 1.5.6 的 `burn_drive_grab`（`libburn/drive.c`）依次做
  打开设备、`start_unit`、介质状态查询（`burn_drive_inquire_media`）与写参数页下发
  （`burn_drive_send_default_page_05`）。`libburn/sg-linux.c` 在打开遇 `EBUSY` 时还
  按 2 秒间隔重试三次。
- 读盘结构。libisofs 在 `-indev` 时把整份 ISO 9660 结构读进来，连只问卷描述符的
  `-pvd_info` 也一样。实测（本机，xorriso 1.5.6，5.2 MB 镜像、1200 个文件）：
  `-pvd_info` 读了 599393 字节的镜像结构（24 次缓冲读），整树列举
  （`-find / -exec lsdl --`）读 599385 字节，两者同一量级。设备路径上这些字节要在
  光驱上取，本机 CD 读取实测约 5 倍速（ADR-0010）。

结论：这不是 Linux 的固有限制，是本仓库在 Linux 上选了子进程增长路线的代价。Windows
快，是因为那边整条准备与写入都在进程内完成。要彻底对齐，只能把 Linux 的追加也切到
原生引擎（ADR-0017、ADR-0020 的写序列），而那条路只在 Windows 真机验证过。

## 决策

1. 新增保守开关 `OPTIBURN_GROW_BACKEND`，取值 `auto`（默认）、`xorriso`、`native`，
   取值判定与错误文案的形状照 `OPTIBURN_READ_BACKEND`（ADR-0024 决策 3）。默认不变：
   Linux `xorriso`（ADR-0006），Windows `native`（ADR-0020）。
2. 开关同时管三件事，保证切换是整条的而不是半条：尺寸预演（`grow_size`）、写入
   （`grow`）、和写前门禁的末区段判定（新增的 `append_gate_session_state`）。
   门禁判定跟随写盘引擎的理由是语义：门禁问的是“将要写盘的引擎能不能读盘上末区段”，
   判定与写入端必须同源。写盘方是 xorriso 时它走读侧分派（xorriso 优先加后端回退，
   ADR-0024 决策 7），写盘方是原生时它走原生读侧。
3. Linux 的默认仍是 xorriso。原生增长的写序列只有在 Windows 真机验证过（ADR-0017、
   ADR-0020），Linux 上只有读侧验证（ADR-0024 补记，Ubuntu 主机加 HL-DT-ST DVDRAM
   GP70N）。这个开关是真机验证与止损的通道，不是默认切换。
4. 不动的东西：写盘的参数表与产物（两个引擎各按自己的布局写）、门禁的三态语义、
   容量门禁的口径与余量（ADR-0019）、读侧五个能力的后端选择（ADR-0024）、
   `repair` 与镜像刻录（`burn`）的平台分派。
5. 新增阶段计时诊断。CLI 的 `burn` 与 `append`、GUI 命令层的刻录与追加任务，在
   `OPTIBURN_TIMING` 置位（非空且不是 `0`）时每段完成往 stderr 打一行，例如
   `阶段 尺寸预演：4.2 秒`。两端的阶段名是同一份文字，可以对着看，GUI 多一段
   `暂存拷贝`（把待刻录文件收进临时目录）。写前窗口的耗时只能在真机上量，计时行
   是这件事的取证工具，也是开关 A/B 的判据。默认关闭，正常输出不加噪声。GUI 的
   输出走 stderr，从终端启动时可见（AppImage 也一样）。
6. `probe` 每台设备多报一行增长模式后端（开关值加实际会用的后端），与读侧后端行
   并列（ADR-0024 决策 4 的形状）。开关值是非法值或在本平台没有可用后端时原样报出。

## 离线验证（2026-10-15，本机，无光驱）

- 开关取值判定、后端命名与分派有单测（`grow_backend_mode_values_and_unknown`、
  `grow_backend_wiring_is_pinned`）。
- 两种夹具上原生判定与 xorriso 判定给同一结论
  （`gate_answers_are_the_same_under_both_writing_backends`：hadris 造的 ISO 盘两侧
  都判可用，纯 UDF 盘两侧都判不可用）。这条钉住的是“换读法不改判定口径”。
- 开关确实换掉了实现。同一台不存在的设备上跑 `grow_size`，`native` 报
  `Mmc(Transport(NotFound("/dev/optiburn-missing-test-device")))`，`xorriso` 报子进程的
  `ReadFailed`（含 xorriso 的 `Drive address ... rejected` 输出）。这条检查要改进程
  环境，与并发测试冲突，因此不留在测试里，结论记在这里。
- CLI 冒烟：`OPTIBURN_TIMING=1 optiburn append …` 在不存在的设备上先打阶段行再报错
  （`阶段 写前门禁：0.0 秒`）。`optiburn probe` 无光驱时仍报“未发现光驱”并退出 0。
- 阶段的开关判定与回落说明都收在引擎一份（`stage_timing_enabled_value` 与
  `GrowSuitability::user_text`），CLI 与 GUI 命令层共用，GUI 只补英文文案；引擎侧有
  `stage_timing_is_opt_in` 一条测试钉住开关口径。
- 全量检查：`cargo test --workspace`、`cargo clippy --workspace --all-targets`、
  `cargo fmt --all --check`、四个目标三元组 `cargo check`，以及 GUI 侧的
  `cargo fmt/clippy/test --manifest-path src-tauri/Cargo.toml` 全绿。

## 真机验证（2026-10-15，HL-DT-ST DVDRAM GP70N LI03，多区段 CD-R）

同一张盘，写前 3 个区段、217 MB 已用、可用 449.0 MB。两档各追加一个小文件（默认档
`默认档测试.txt` 72 字节，原生档 `原生档测试.txt` 60 字节），数字取自
`OPTIBURN_TIMING=1 optiburn append` 的阶段行：

| 阶段 | 默认档（xorriso） | 原生档（`OPTIBURN_GROW_BACKEND=native`） |
|---|---|---|
| 写前门禁 | 5.6 秒 | 0.2 秒 |
| 尺寸预演 | 42.1 秒 | 0.1 秒 |
| 容量检查 | 3.3 秒 | 0.0 秒 |
| 写入 | 30.4 秒 | 16.5 秒 |
| 合计 | 81.4 秒 | 16.9 秒 |

点下去到开始写数据之间：默认档 51.0 秒，原生档 0.3 秒。报告的“十几秒”在这张盘上是
51 秒，且几乎全部落在尺寸预演那一次子进程上（它要重新初始化驱动器，再把旧区段的整份
ISO 结构读一遍）。原生档写前的三段合计 0.3 秒，与不再起子进程一致。原生档的写入本身
也短了一截（30.4 对 16.5 秒，同一个小会话，差异来自倍速选择与写序列，未逐条取证）。

核对到的：

- 两档追加的文件都从盘上读回，内容与写前逐字节一致。
- 嫁接的旧文件数据逐字节核对通过。末区段（原生档那次写的，嫁接链跨两档两次写入）
  上的 `OptiBurn_0.1.7_x64-setup-offline.exe`（220356536 字节）sha256 与 Release
  v0.1.7 的同名产物摘要一致（`6c142de4547dd07c2578d9742ce2307e2fc91f7a05d95c9c0140f7dc23dfe569`），
  `常用字体.zip`（7 MB）与 `建议书2.1.3部分.docx` 的 `unzip -t` 报
  `No errors detected`，`20261015 初稿 主报告提纲.docx` 是 OLE2 复合文档（不是
  zip，走头字段与大小核对）。嫁接式增长的原地引用地址正确。
- 两次写入后末区段的目录树里原有 4 个文件仍在，区段数从 3 增到 5（每档一个区段），
  盘仍可追加，可用容量 449.0 降到 420.8 MB（一个区段约 28 MB 开销，含链接块与导入
  导出区，与 ADR-0019 的 16 MB 余量同量级）。
- `probe` 的增长模式后端行与开关一致（`auto` 报 xorriso 增长模式，原生档那次的前置
  耗时是默认档的三段里任何一段的十分之一以下）。
- `probe` 报的区段数比 xorriso 的 `-toc` 多 1（写入后 5 对 4）。写入一个未关闭的区段
  之后，固件的计数把 NWA 处的隐形结构也算进去，与 ADR-0019 说的 `READ DISC
  INFORMATION` 字节 6 同源，属既有行为，不是本次写入多出一个区段。

未了事项：

- 无。下面这条已定性并修复。

名字问题（末区段里 3 个中文名成了替代名）的定性：

- 逐区段比对（`-load session <n>` 配 `-read_fs norock` 与 `-read_fs nojoliet`）：
  区段 1 与 2 的 Joliet 名都是真名。区段 3（写入区段 2 之后那一次，早于本次两次验证
  写入）里继承来的 3 个中文名在两个命名空间都已是替代名，而它自己新增的那一条在
  Rock Ridge 侧是真名、Joliet 侧是替代名。区段 4 与 5（本次两次写入）只是把区段 3
  已经变坏的名字继续带下去。
- 这个形态对应的是：xorriso 按调用方 locale 猜本地字符集，非 UTF-8 环境里 libisofs
  在**读旧区段**时把 Joliet 名换成替代名（继承条目两个命名空间都坏），而**新加文件**
  的名字以本地字节形式落在 Rock Ridge 侧（真名）、Joliet 侧转换失败（替代名）。
  本机离线按同一形态复现（`LC_ALL=C` 跑一次增长，见 ADR-0026 的背景一节）。
- 修复：会碰文件名的 xorriso 调用固定 `-local_charset UTF-8`（ADR-0026），非 UTF-8
  环境不再写坏名字。已经写在盘上的替代名不可回修（一次写介质），要恢复只能按原名
  重新追加这些文件。文件数据从头到尾没受影响。

## 被否决的方案

- 读侧整体切原生（同 ADR-0024 被否决的理由）：ISO 盘的名字解析与宽松度会变，需要一次
  Linux 真机全量回归。
- 门禁改原生优先、写入端仍是 xorriso：省一次子进程，但门禁的结论与写入端脱钩。
  libisofs 读不出的盘上原生读侧可能读得通，门禁会放行，而追加出来的新区段看不到旧
  区段里的文件（ADR-0019 的门禁拒绝正是防这个）。
- 去掉尺寸预演改成源目录文件大小求和：同 ADR-0019 被否决的理由（小文件多的目录里
  显著低估）。
- 预演与门禁合并成一个 xorriso 进程：预演本来就要读旧区段，能顺带回答门禁的问题，
  省一次子进程且判定口径不变。代价是损坏末区段的提示会挪到暂存拷贝之后（GUI 会先
  白拷一遍用户文件），且要把 `-print_size` 的 stderr 兜底空镜像信号接进门禁的三态
  分流。这条改动跨两个前端的写前流程，没有真机数据前不动。
- 三次调用并进一个常驻 `-dialog` 会话：每次 `-dev` 与 `-indev` 仍要重新 grab 驱动器，
  省下的只有进程启动，改动面却遍及整个写前流程。

## 后果

- 默认行为与产物不变，Linux 的追加准备仍是三次 xorriso 调用。这次交付的是解释与
  取证手段（本 ADR 加 `OPTIBURN_TIMING` 的计时行），以及一条能立刻绕开这段等待的
  通道：真机上默认档写前 51.0 秒，原生档 0.3 秒。
- 置位 `native` 后 Linux 的写前窗口里不再有子进程，与 Windows 同路。原生增长的写
  序列已在 Linux 真机验证过一次（CD-R 盘上追加一个区段，内容读回一致），DVD±R 与
  BD-R 待介质。原生增长在可覆写介质上不支持增长（`NativeGap::GrowthOnRewritable`，
  与 Windows 一致），这类盘要么用默认档，要么换盘。
- 文档同步：`docs/ARCHITECTURE.md` 的增长模式分派与平台表、`CONTEXT.md` 的保守开关
  与阶段计时两条、`CHANGELOG.md`。
- 后续可做。真机数据支持把 Linux 的默认切到原生增长（写前 51.0 秒对 0.3 秒）。两个前置条件都已具备：嫁接数据的逐字节校验通过（见上），替代名问题已定性并修复（ADR-0026），且那两次写入的会话在验证里被完整读回。
  ADR-0027 已经把默认切过去：`auto` 两边都走原生，Linux 按设备事实回退 xorriso（随机可写介质、未知 Profile、指定倍速三条）。本开关因此从“切到原生”变成“显式选择或回退”。
  预演与门禁合并（见被否决的方案）只在显式 xorriso 与回退那几档还有价值，量级不足以再做。
