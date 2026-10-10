# ADR-0022：损坏末区段的回退与续写

- 状态：已接受
- 日期：2026-10-11

## 背景

刻录中断（USB 供电、误拔线、断电）会在盘上留下未完成的轨道或未关区段。ADR-0017
记过一种形态：驱动器把未完成的轨道排除在 TOC 之外，NWA 跳过它，盘照常可读可写，
本工具本来就能用。2026-10-11 的一次现场拔线复现了这个形态：区段数不变、TOC 的末
区段起点仍指向上一个完整会话、读盘列出 26 个条目、卷标还是旧区段的。

另一种形态现在整盘不可用：固件把残片登记成末区段（例如崩在关区段过程中），读它的
卷描述符区或目录树失败。读盘报错，追加报「末区段不是 ISO 9660」，而更早区段里的
数据仍完好、剩余空间也仍可写。issue #40 要的是把这类盘救回来。

## 决策

1. 候选枚举：`READ TOC/PMA/ATIP` 的 Format 0（轨道列表，新增到 `optiburn-mmc`）给出
   所有轨道起点，Format 1 给出末区段起点；两者并集去重（去掉驱动器报的导出区
   0xAA）后按新到旧排序。每个区段的首条轨道起点就是该区段起点，区段内的后续轨道会
   被验证淘汰。镜像文件的候选只有一个，起点 0。
2. 逐候选验证：卷描述符区要有 `CD001` 与 PVD（沿用 ADR-0018 的探测），再把整棵目录
   树走一遍，要求每条记录都能解析、目录数据都读得到。走不通即淘汰，读失败也算淘汰，
   设备路径上能读到哪里由驱动器决定，越界读自然暴露成错误。按 issue #40 的决策，
   验证强度只到目录树，不逐文件抽读。
3. 最新的可用候选获胜。读盘的四个能力与卷标都走这个选择器
   （`open_readable_session`），回退信息（跳过几个候选、选中第几个、起点）随之报出。
   全部候选都不可用时返回**最新候选**的错误，读盘文案与改动前一致。
4. 追加的嫁接源用同一套枚举（`read_graft_source`），写入位置仍是 NWA。末区段读不
   出来而更早区段可读时，没有用户确认就报 `BurnError::DamagedLastSession` 拒绝：
   被跳过区段里的文件不会进新区段的目录树，等于从可见视图消失。确认由调用方表达，
   CLI 是 `--allow-damaged-last-session`，GUI 追加页是「允许跳过损坏的末区段」。
5. 追加门禁从「末区段是不是 ISO 9660」（布尔）升级为三态 `IsoSessionState`：末区段
   可用、末区段损坏但更早区段可用、没有可用的 ISO 会话。前两者的差别正是「能不能
   救」与「该不该问」。Linux 的 xorriso 路径没有候选回退，损坏末区段在那边归「没有
   可用会话」，与改动前行为一致。
6. 平台差异写在明处：回退只在 Windows 的原生读侧与原生增长上生效。Linux 侧维持
   xorriso 的行为，理由与 ADR-0018 的后果节同一条。

## 后果

- 刻录中断的盘分两种结局。更早区段完好时读总能救回来（读侧回退到最新的可用会话，
  被跳过区段里的文件从可见目录消失，数据仍在盘上但没有目录指向它们）。续写取决于
  驱动器：NWA_V 仍置位时可以直接续写；Damage 置位时先按 libburn 的顺序尝试关闭损坏
  的轨道与区段，成功后（NWA_V 恢复）才能继续；驱动器拒绝修复时这张盘就是
  "Damaged, not closed and not writable"，只能读（真机实测，见补记）。
- 读盘对这类盘是透明回退：列出的文件来自最新的可用区段，卷标同理，用户看到的树比
  物理上盘上的内容少。回退信息经 `iso_session_state` 报给调用方，浏览路径的提示
  记在 issue #40 的后续（本轮只做了追加门的确认）。
- 门禁的名字与语义变了（`last_session_is_iso` 换成 `iso_session_state`），CLI、GUI
  与测试里的调用点全部迁移。
- 验证：合成盘单测覆盖四种情形（残片末区段加完好前段、描述符完整但目录树损坏的
  残片、全部候选不可用、健康盘不报回退），追加路径另有两条（无确认拒绝、确认后
  嫁接源换成更早区段且仍写在 NWA）。真机回归用同一张 CD-R 验证健康盘行为不变：
  追加后 10 个区段、27 个条目、卷标 `RECOVERY`、TOC 的末区段起点指向新会话。
- 未验证：目标形态（固件把残片登记成末区段）没有在真机上复现出来。那次现场拔线落
  在宽容形态（残片不进 TOC），说明要崩在关区段过程中才会出现，时序不好掐。合成盘
  夹具按 MMC 与内核的语义构造，真机证据待有这类盘时补。
- `READ TOC Format 0` 的解析夹具取自实测的 9 区段 CD-R 响应：`data_len=82`、首末
  轨道 1 与 9、十条描述符（含 0xAA 导出区）、轨道起点 0 到 287070。

## 补记：Linux 侧的参考实现与真机结论（2026-10-11）

维护者指出这条路径在 Linux 上已有实现，源码级对齐（libburn 1.5.6 与 xorriso 1.5.6
的源码与手册）得到两件东西：

1. **判定**：libburn 的 `mmc_get_nwa`（`libburn/mmc.c`）读 `READ TRACK INFORMATION`
   的响应字节 5 bit 5（MMC-5 6.27.3.7 的 Damage 位）与字节 7 bit 0（6.27.3.9 的
   NWA_V），按四种组合分流：Damage 置位且 NWA_V 清零是 "Damaged, not closed and
   not writable"，Damage 置位而 NWA_V 仍置位是 "Damaged and not closed"，只有 NWA_V
   清零是 "No Next-Writable-Address"，都伴随 `next_track_damaged` 状态位；xorriso 把
   它呈现在 `-toc` 的 Media status 行（"but next track is damaged"）与驱动器接管时的
   警告里。
2. **修复**：libburn 的 `burn_disc_close_damaged`（`libburn/write.c`）在驱动器报了
   Damage 位时尝试关闭损坏的轨道与区段：CD 与 DVD-R 族先下发写参数页（CD 用 TAO，
   DVD-R 用增量写）再关区段；DVD+R 与 BD-R 关最后一条轨道（按 `multi` 决定是否连
   区段一起关）。xorriso 用它做 `-close_damaged as_needed|force`，`as_needed` 只在
   驱动器报损坏时动，`force` 无条件尝试，手册明说“这可能适用于 CD-R、CD-RW、DVD-R、
   DVD-RW、DVD+R、DVD+R DL 或 BD-R”。

本仓库按同一口径实现（Windows 原生侧）：`TrackInfo` 补 `damaged` 与 `nwa_valid`
两个位，写前路由只信 NWA_V 置位的地址字段，位清零时按 `close_damaged` 的顺序尝试
修复再复读一次，仍拿不到地址就按 `BurnError::WriteAddressUnknown` 拒绝并区分两种
文案；`optiburn repair --device <设备> [--force]` 是显式入口，等价于
`-close_damaged as_needed|force`；Linux 侧直接调 xorriso 的 `-close_damaged`。

CDB 编码也照 libburn 的 `mmc_close` 修正：`CLOSE TRACK/SESSION` 的功能码是字节 2 的
位 2-0（`(session & 3) << 1 | !!track`，0b001 关轨道、0b010 关区段），轨道号是字节
4-5 的 **16 位大端**字段。本仓库早期只往字节 4 填轨道号，实测被驱动器以 INVALID
FIELD IN CDB 拒绝（等于在关轨道 0）。`READ TRACK INFORMATION` 的轨道号同理：早期把
32 位写进字节 2-5，低字节落进保留的字节 5，实测任何轨道号都回地址全零的退化描述符；
字节 1 也不能置位（同一台驱动器回 INVALID FIELD IN CDB）。改到「字节 1 置 0、轨道号
在字节 2-3」后，同一张盘回出真实描述符：轨道 0xFF 给开放轨道，起始 301170。

**真机结论（同一张被拔线的 CD-R，HL-DT-ST GP70N）**：所有修复路径都被驱动器拒绝——
`WRITE(10)` 到开放轨道地址报 ILLEGAL REQUEST 加地址越界，`CLOSE TRACK/SESSION`
功能码 2 与 6 报 SESSION FIXATION ERROR（ASC 0x72/ASCQ 0x03），功能码 0、1、3、4、5、7
报 INVALID FIELD IN CDB，`REPAIR TRACK/SESSION`（0x58）整条命令不实现（INVALID
COMMAND OPERATION CODE）。按 libburn 的口径这张盘就是 "Damaged, not closed and not
writable"：**软件能把状态判准、能把修复尝试做全、不能把这份固件状态变回可写**。盘上
已有数据仍完整可读（读侧回退到最新的可用会话，27 个条目、卷标 RECOVERY 未被破坏）。

顺带更正一处早期结论：`READ TRACK INFORMATION` 的 NWA 字段与起始地址都不可单独当作
写入位置——健康盘上两者相等，损坏盘上两者都可能为零或不可信，只能以 NWA_V 位为准
（libburn 的取法），本仓库早期用「两个字段取大者」是一种推测，已废弃。

## 被否决的方案

- 按块扫全盘找 ISO 描述符区：代价高（700 MB 盘 35 万块），而 TOC 的轨道起点已经是
  区段起点集合的超集。
- 把残片当成可用区段继续写：目录树读不出来，硬写会让新会话引用不确定的 extent。
- 自动回退不询问：被跳过区段里的文件会从可见视图消失，属于数据可见性的改变，必须
  由用户决定（issue #40 的待决策 2 定为要确认）。
- 读不出末区段时一律按改动前的文案拒绝：那会把可救的盘一并拒掉，正是要解的问题。
- Linux 侧同时接回退：xorriso 没有候选概念，要接就得把读侧整体切到原生
  （ADR-0018 未做，issue #41 是相邻话题），本轮不做。
