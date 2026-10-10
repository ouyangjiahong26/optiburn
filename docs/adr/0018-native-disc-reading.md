# ADR-0018：原生读盘（v0.6）

- 状态：已接受
- 日期：2026-10-10

## 背景

读侧（读卷标、末区段 ISO 门禁、列目录树、整树抽取、按路径抽取）此前只有 xorriso
一条路（ADR-0010、ADR-0012）。Windows 上没有能访问光驱的 xorriso 构建（ADR-0008
补记），GUI 的设备页浏览、复制、回读校验在 Windows 全部不可用，报“缺 xorriso”。
写侧已由原生 MMC 引擎解决（ADR-0017），读侧缺的是同一块：MMC 读块加 ISO 9660
解析。`hadris-iso`（MIT，已在依赖树里，mastering 用它的写侧）自带读侧（`IsoImage`：
描述符解析、目录遍历、Joliet、RRIP、多 extent 文件），只需要喂给它一个按区段
正确寻址的字节源。

## 决策

1. 读侧接缝 [`ReadBackend`]（五个方法，`BurnEngine` 的读侧对偶）：`XorrisoRead`
   包住原有实现，`NativeRead` 是新路径。五个公开函数改成薄分派，规则与刻录引擎
   相同：Windows 走原生（那边 xorriso 走不通），Linux 维持 xorriso。这是“哪条路
   走得通”的平台差异，不是设备语义差异。Linux 是否切到原生另见后果节的评估结论。
2. 末区段定位用 READ TOC Format 1（区段信息）的末区段起始地址。libburn 取同一
   份（`MMC_GET_MSINFO`），CDB 逐字节对齐。可追加盘上 READ DISC INFORMATION 的
   字节 5（末区段首轨）指向开放区段的隐形轨道，读那里落在空区，不能用（实测见
   补记）。
3. 区段地址约定两种都认，按“根目录首记录是否自引用”探测：
   - 区段相对（自家原生引擎写出的区段，独立镜像原样落盘）：目录记录里的 extent
     加区段起点。
   - 盘级绝对（xorriso 增长模式的区段，libisofs 的 ms_block 位移语义）：extent
     原样当盘上 LBA，描述符区（逻辑块 16 起）例外，仍落在区段起点加偏移。
   - 探测读根 extent 所在块，比较首条目录记录（“.”）的 extent 字段与所在块号，
     两种约定下该字段都指向根目录自身，只有解释对得上时相等。相对候选先试，
     读取失败（越过可读范围）软处理，换绝对候选再判。
4. 解析用 `hadris-iso` 的 `IsoImage`（依赖加在 engine，只开 std、sync、read、
   joliet 四个 feature）。命名空间按 hadris 的优先级取（RRIP 优先于 Joliet 优先于
   平面名），Joliet 名按 UTF-16BE 解码（hadris 的 display_name 不解 Joliet）。
   卷标取 PVD 的卷标识，为空回退 Joliet SVD 的 UTF-16BE 值。
5. 读侧文件名与路径落盘前过既有 [`safe_relative_path`] 的安全过滤（盘内容不是
   可信输入，ADR-0010 的规则不因后端不同而放松）。取消在每次设备读与每个目录
   条目处检查。取消经 io 错误上抛时不能用 `ErrorKind::Interrupted`（hadris 的
   read_exact 会无限重试它），用令牌状态归因。
6. 盘上没有 ISO 9660（空白盘、UDF 盘、音频轨）报 `NoIsoSession`，与 xorriso 路径
   识别兜底空镜像的口径一致，`last_session_is_iso` 把它翻成“不是”。空白盘在设备
   路径上先查 READ DISC INFORMATION 的状态位再发 TOC：没有已完结区段时 TOC 按
   驱动器不同可能回短数据也可能报命令失败，先查状态才不会把空盘当设备错误。

[`ReadBackend`]: ../crates/optiburn-engine/src/readback.rs
[`safe_relative_path`]: ../crates/optiburn-engine/src/readback.rs

## 后果

- Windows 上读侧不再依赖外部程序：设备页浏览、回读校验、追加页卷标预填、
  CLI `append` 的末区段门禁全部可用。“复制选中文件”仍仅 Linux：抽取这步走得通，
  最后一步写系统剪贴板在非 Linux 平台返回错误（ADR-0012）。`MissingTool` 在
  Windows 读路径上不再出现。
- Linux 维持 xorriso 读侧（2026-10-10 评估结论）：那边 xorriso 的安装与权限路径
  成熟，用户现有行为不变；原生读侧只在 Windows 启用。这是维护者决策，不是技术上
  走不通（本机没有 Linux 光驱可做真机对拍，切换收益不足以承担一次全量回归）。
- 读块走既有的 `MmcDevice::read_blocks`（READ(10)，32 块一条，SPTI 单次传输上限，
  ADR-0017 补记），批量内部缓冲 64 KiB，顺序读时每块一条命令。
- 镜像文件走同一条解析路径（`FileBlocks`，起点 0、区段相对约定），回读校验在
  Windows 上对镜像与盘片两侧都不再要 xorriso。
- 初始记的四处缺口已经收口：
  - UDF 盘读侧：纯 UDF 盘与多区段 UDF 盘的末区段都能读（`disc_read/udf.rs`，
    两种地址约定按 AVDP 的 tag_location 区分），决策与能力边界见 ADR-0021。
  - 原生增长模式：Windows 上用嫁接式会话生成器实现，决策见 ADR-0020。
  - 写侧位移：由生成器承担。增长会话直接生成盘级绝对地址（ADR-0020 决策 2），
    镜像刻录路径在所有可达调用点都写 LBA 0（单区段镜像），因此不再产生新的
    区段相对会话；区段相对约定只作为读侧兼容旧盘的一档保留。
  - Linux 验证：见上面的维持结论。
- 仍开着的缺口只有两项：Rock Ridge 名的显式优先策略（当前按 hadris 的
  best_choice，与“Joliet 优先”的实际差异只在两者同存且都有效时的名字来源）、
  非 CD 介质上区段信息的假值风险（MMC-5 6.26.3.3.3 允许驱动器对非 CD 回 track 1、
  LBA 0 的无用假值，实测的 CD-R 驱动器回真值，DVD/BD 未验证，遇假值会静默定位到
  首个区段。原生增长的写前路由依赖同一条 TOC 信息，因此这条也牵到追加）。

## 被否决的方案

- 让 Windows 用户装能访问光驱的 xorriso 构建：需要自行维护链接 libcdio 的原生
  构建（ADR-0008 补记已否决）。
- 只支持区段相对约定、绝对约定报错：盘上已存在 xorriso 增长区段（Linux 用户的多
  区段盘、本仓库自测盘），读不出它们等于读盘能力不完整。
- 按区段起点重写解析器自己解 ISO 9660：hadris-iso 的读侧已经在依赖树里，自写
  一份只有维护成本。
- 取消用 `ErrorKind::Interrupted` 表达：hadris 的 `read_exact` 按约定无限重试
  Interrupted，会死循环（其文档写明 retrying interrupted operations）。

## 补记：真机实测（2026-10-10，Windows，USB 光驱 D:，可追加 CD-R，8 区段）

- READ TOC Format 1 只回一条描述符（末个可读区段），8 区段的盘数据长度仍是 10。
   描述符的起始地址与轨道表交叉验证一致（描述符给 279570 时，Format 0 的轨道表
   里同地址正是末条数据轨的起点）。响应里首末会话编号比 READ DISC INFORMATION
   的区段数少 1（开放区段不计入），解析时不用对齐这两个口径。
- READ DISC INFORMATION 字节 5 的实测行为：刻录前后各读一次，它始终指向 NWA 处
   尚未写入的隐形轨道（8 轨时给 9，写入后给 10），轨道信息查询返回的 start 与
   NWA 相等，盘上该位置没有数据。末区段定位不能用它。
- 两种区段地址约定的实测证据：xorriso 增长的区段（起点 150093）PVD 里根 extent 是
   150112（绝对，等于起点加 19），而卷空间大小是 107575（区段相对大小，不是
   绝对卷尾）。所以 extent 与 vss 的约定不同源，源长度一律按“区段起点加 vss”
   算。自家原生引擎刻的四个区段（NATIVETEST）全部按区段相对约定读通。
- 描述符区永远在区段起点加 16：8 个区段（4 个 xorriso、4 个原生）逐一验证，
   全部在起点加 16 处有 CD001。绝对约定下批量读块不能跨过描述符区边界（映射
   在那里不连续），内部缓冲在那里截断。
- 端到端：末区段（原生刻的 NATIVETEST）读卷标、列目录树、整树抽取，从盘上与
   从镜像文件各抽一棵树逐文件对拍一致。xorriso 增长的旧区段按绝对约定读出
   卷标 OPTIBURN 与 7 个条目。离线对拍：同一镜像文件上原生列举与
   `xorriso -find / -exec lsdl` 的路径与大小逐条一致（含中文名）。
