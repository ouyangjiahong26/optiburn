# ADR-0029：非 CD 介质的区段定位改走轨道信息

- 状态：已接受
- 日期：2026-10-22

## 背景

维护者在一台 HL-DT-ST DVDRAM GP75N 上第一次用 DVD-R 刻录，写盘前的门禁报
`读取末区段格式失败`，附带 `SCSI command [43, 00, 01, ...] failed: status=2,
sense=[70, 00, 05, ..., 24, 00]`。这条 CDB 是 READ TOC/PMA/ATIP 的 Format 1
（区段信息），sense 是 ILLEGAL REQUEST 加 ASC 0x24（INVALID FIELD IN CDB）。

同一台驱动器换 CD-R：Format 0 与 Format 1 都正常回数据。同一张 DVD-R 上直接下发
两条命令（本机实测）：

| 命令 | DVD-R | CD-R |
|---|---|---|
| READ DISC INFORMATION（0x51） | 成功，可追加、1 个区段、末个区段未完成 | 成功 |
| READ TOC Format 0（0x43） | ILLEGAL REQUEST / 0x24 | 成功 |
| READ TOC Format 1（0x43） | ILLEGAL REQUEST / 0x24 | 成功 |
| READ TRACK INFORMATION 轨道 1 | LBA 越界（盘上尚无轨道） | 成功 |
| GET CONFIGURATION 当前 Profile | `0x0011` DVD-R | CD-R |

xorriso 在同一个驱动器上读这张盘也报 `Cannot obtain Table Of Content`，独立佐证这
不是本仓库的 CDB 拼装问题。Format 1 的区段信息只有 CD 定义，DVD 与 BD 的区段与
轨道信息走 READ DISC INFORMATION 与 READ TRACK INFORMATION。

后果不止是读盘报错：写盘后的回读校验、追加门禁、增长模式的末区段定位全部依赖
`read_toc_session_info`，于是 DVD 介质上这些路径一起失败。维护者那一盘的数据其实
写成功了，回读步骤报错让整次刻录被当成失败，盘上文件完好（抽出来核对过 sha256）。

## 决策

1. `MmcError::is_illegal_request` 认 sense key 5（ILLEGAL REQUEST）。它表示“这条
   CDB 或它的参数对当前介质没有定义”，与传输故障、介质错误分开。DVD-R 上实测
   到两种 ASC：READ TOC 回 0x24（INVALID FIELD IN CDB），READ TRACK INFORMATION
   问轨道 0xFF 回 0x21（LBA OUT OF RANGE）。只认固定的 ASC 会把后者漏掉。
2. `read_toc_session_info` 遇到它回 `Ok(None)`，`read_toc_tracks` 回空列表。语义
   与既有约定一致：拿不到区段信息不是命令失败，调用方自己兜底。其它 sense（例如
   介质错误 0x03）照常上报。
3. 新增 `disc_read::track_starts`：按 READ DISC INFORMATION 报的轨道范围逐个问
   READ TRACK INFORMATION，取轨道起点。轨道号全问不出来时，单区段盘按起点 0 试
   一次（这类盘不可能有前序区段）。
4. `session_candidates` 在 TOC 拿不到任何候选时用 `track_starts` 兜底，命名与排序
   规则不变（去重、新到旧、导出区不参与）。
5. 增长模式与抢救的末区段起点统一收在 `disc_read::last_session_start`：先问 TOC
   Format 1，取不到再用轨道起点里的最大值。
6. 保守开关 `OPTIBURN_LAST_SESSION_ONLY` 的语义不动：置位后仍然只认 TOC 给出的
   末区段，DVD 上等于没有候选。

## 后果

- DVD-R 与 DVD+R 上的读盘、回读校验、追加门禁、增长模式不再因为 TOC 被拒而失败。
- CD 路径行为不变：TOC 正常时不会走到轨道兜底。
- 单区段 DVD 的候选只有一个起点 0，与镜像文件的处理一致。
- 已知边界：多区段 DVD+R 的候选是各轨道的起点，与 CD 上“每个区段的首条轨道起点
  就是区段起点”同一口径，但这类盘的多区段形态还没在真机上验过，验证时会连同
  `session_candidates` 的候选顺序一起核对。

## 被否决的方案

- 在引擎层捕获错误再兜底：`read_toc_session_info` 的调用点有五处（读盘、增长、抢救、
  UDF 读侧、容量探测），逐处判断会漏，且每处都要重复认 sense。
- 用 Profile 判断介质再决定是否发 TOC 命令：驱动器的 Profile 是静态能力表，与实际
  介质未必一致，而 sense 是设备对这条命令的直接答复。
- 在门禁上把 DVD 直接放行：读盘、回读校验仍然会失败，问题只是从门禁挪到别处。
