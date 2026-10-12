# ADR-0024：读侧后端回退

- 状态：已接受
- 日期：2026-10-11

## 背景

读盘的五个能力（卷标、末区段格式门禁、目录树列举、整树抽取、按路径抽取）在 Linux 恒走 xorriso 子进程（ADR-0018 的分派规则）。xorriso 1.5.6 的读侧只解析 ISO 9660，纯 UDF 盘在它眼里是“没有 ISO 9660 的空镜像”：实测（2026-10-11，Ubuntu，xorriso 1.5.6）对 hadris-cd `udf_only()` 的夹具（UDF 1.02，卷识别序列从逻辑块 16 起、锚点 256）报 `No ISO 9660 image at LBA 0. Creating blank image.` 后照常退出（退出码 0），卷标给空镜像假值 `ISOIMAGE`。纯 UDF 盘是 Windows 系统自带刻录的默认输出（ADR-0021），Linux 侧读不了它等于读盘能力不完整。

五个能力的失败形态不统一：卷标与目录树列举报 `NoIsoSession`，末区段门禁给 `Unusable`，整树抽取成功但目标目录零文件，按路径抽取报“找不到文件”。空镜像兜底信号在五条路径的 stderr 里都出现，谓词 `is_blank_image_fallback` 已经在识破它。合法空目录的 ISO 盘同样列零条目、抽零文件，但没有这个信号（实测对照）。

原生读侧在 Linux 上零件齐全：SG_IO 传输、`MmcDevice::read_blocks` 与平台无关的 ISO 9660、UDF 解析，同一张纯 UDF 夹具在 Linux 本机读通（卷标 `UDFONLY`、六个条目含中文名、抽出文件的 md5 与 ADR-0021 补记的独立工具对拍值一致）。缺的只是分派。

## 决策

1. 分派从二选一改成后端回退：Linux 的读盘能力先走 xorriso，出现“xorriso 读不到 ISO 9660”的信号时改用原生读侧重试同一次操作。触发信号两种：xorriso 输出报空镜像兜底（`No ISO 9660 image` 或 `Creating blank image`），或读侧报错。空结果不单独触发。整树抽取与按路径抽取改成收集 stderr 后判信号，stderr 缺失时不回退。
2. 触发边界：xorriso 缺失或起不来（`MissingTool`）不触发后端回退，维持安装提示；真空白盘（`Media status : is blank`）不触发，按现状归 `NoIsoSession`。
3. 保守开关 `OPTIBURN_READ_BACKEND`：`auto`（默认，xorriso 优先加后端回退）、`xorriso`（退回现状，纯 xorriso）、`native`（直接原生）。取值判定照 `OPTIBURN_LAST_SESSION_ONLY` 的形状（空值算默认）。两平台都认这个开关，Windows 遇 `xorriso` 值报“没有可用的 xorriso”。
4. 两个后端都失败时报原生侧的错误，两平台同盘同文案，不并列 xorriso 的错误。后端回退的事实不进任何面向用户输出，也不建诊断记录：真机排查用保守开关做 A/B 对跑，`probe` 每台设备多报一行读侧后端现状（开关值加实际会用的后端）。
5. ISO 盘与 Bridge 盘维持 xorriso 优先，本轮不整体切原生。
6. 抢救（salvage）随后端回退一并打通：Linux 走原生读侧，xorriso 看不见未关闭轨道（ADR-0022 补记），那条按决策 1 必然落到原生。
7. 末区段格式门禁的语义不变：`IsoSessionState::Unusable` 的判定口径不因后端回退改变，纯 UDF 盘照旧不能续写（ADR-0010、ADR-0020）。

## 后果

- Linux 上纯 UDF 盘的卷标、目录树列举、整树抽取、按路径抽取可用，与 Windows 原生读侧等价，抢救在 Linux 一并可用。
- 损坏末区段的候选区段回退（ADR-0022）在 Linux 顺带可用：触发条件含“读侧报错”，落到原生后候选枚举与验证自动生效。这是预期收益，不是额外改动，测试里要有它。
- 保守开关置位 `xorriso` 时行为与本次改动前一致，是对照与止损的退路。
- 验收分两级：镜像分支（合成盘夹具）全绿即合并，真机证据（一张纯 UDF 实物盘、卷标与目录树列举与抽取三类读操作、与 `udfinfo` 加内核 `mount -t udf` 的逐字节对拍、设备路径上触发信号形态的确认）由维护者在接了光驱的主机上补，结果记回 issue #41。设备路径的触发信号形态目前只有镜像文件的证据。
- 文档同步：ADR-0018 与 ADR-0021 的后果节各加一行指向本 ADR，`docs/ARCHITECTURE.md` 平台表的读盘一行改写，`CONTEXT.md` 立纯 UDF 盘、后端回退、保守开关三条术语。

## 被否决的方案

- Linux 读侧整体切原生：一套读法、平台差异消失，但 ISO 盘的名字解析与宽松度会变，需要一次 Linux 真机全量回归，而本机没有 Linux 光驱。
- 等 xorriso 或 libisofs 支持 UDF 读：上游只有 UDF 写支持，不掌握主动。
- 成功但结果为空也算触发：合法空目录的 ISO 盘会误触发，回退后原生读侧要求 SG_IO 权限，没权限的用户会从正常返回空变成报错。
- 每次操作前先探测末区段是不是 ISO 9660 再选后端：多一次设备往返，且末区段门禁的 `Unusable` 语义不能挪作探测用。
- 后端回退的事实走日志或改返回类型带给调用方：核心 crate 没有日志设施，为一条诊断引一套设施或改返回类型不成比例，保守开关已经是排查工具。

## 补记：设备路径真机验证（2026-10-12，Ubuntu 主机 + HL-DT-ST DVDRAM GP70N）

维护者放入一张多区段 CD-R（`blkid` 报 `iso9660`、Joliet、`LABEL=RECOVERY`，9 个区段，`READ TOC` Format 1 报末区段起点 293970，驱动器报 `next track is damaged`）。这张是 ISO 9660 盘不是纯 UDF 盘，验证的是设备路径上的后端回退接线，纯 UDF 盘的读取仍待实物盘。

观测（三个开关值各跑一遍，`OPTIBURN_READ_BACKEND` 分别取 `auto`、`xorriso`、`native`）：

- 卷标 `RECOVERY`、列举 27 条、整树抽取 25 个文件、抢救未关闭轨道（起点 301170，边界 310423）`26` 个文件（完整 25，半截 1）三档都可用；`auto` 与 `native` 抽出的清单与内核挂载同一张盘（`/run/media/<用户>/RECOVERY`）逐条一致，共同文件的 md5 一致。
- 抢救这条在改动前 Linux 恒不可用，本轮机验证实 `auto` 与 `native` 都走通，`xorriso` 值按设计返回“看不见未关闭轨道”。
- 修掉两处只有设备路径才踩得到的前置缺陷（真机复现，两道都不是纯 UDF 特有）：
  - `disc_read::classify_source` 只认 Windows 盘符，`/dev/sr0` 被判成镜像文件；块设备 `stat` 长度是 0，原生读侧没读一个字节就归 `NoIsoSession`，设备路径上的原生读取、UDF 读取与抢救因此全部失效，后端回退也打不开设备。现按 `/dev/` 前缀一并认作光驱。
  - `readback::extract_args` 少了 `-drive_access shared`。盘被桌面环境自动挂载时 xorriso 独占打开失败（`Cannot open busy device`，实测），整树抽取回落原生；补上与其它只读参数表同一口径。
- 未了事项：
  - 纯 UDF 实物盘在 Linux 上的读取仍无真机证据，触发信号的形态目前只在镜像文件与这张 ISO 盘上确认。
  - xorriso 在 `LC_ALL=C` 下把 Joliet 的非 ASCII 名换成替代名（`/中文追加.txt` 变 `/__e______._t_x...`，libisofs 报 `Cannot convert from charset UTF-16BE to ANSI_X3.4-1968`），原生读侧按 Joliet 读得出原名。这是 xorriso 优先这一档的既有代价（改动前就成立，非本轮引入），只在此记下，分派与字符集设定未动。
