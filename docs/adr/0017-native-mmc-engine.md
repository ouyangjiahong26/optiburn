# ADR-0017：原生 MMC 写引擎（v0.5）

- 状态：已接受
- 日期：2026-10-10

## 背景

0.1.5 之前刻录只有一条路：`xorriso -as cdrecord` 子进程（ADR-0004）。首次在 Windows
上做真机测试时发现这条路在那边根本走不通：MSYS2 的 xorriso 编译时没链 libcdio，不含
MMC 传输层，装了也读不了盘、刻不了录（证据见 ADR-0008 补记）。Windows 的 GUI 目标用户
（ADR-0008）因此没有任何可用的刻录路径。同时本机现在有了 USB 光驱（HL-DT-ST DVDRAM
GP70N）可做真机验证，ADR-0008 当年“本机没有可验证环境”的理由不再成立。

## 决策

1. `optiburn-mmc` 增加写侧命令：GET CONFIGURATION、MODE SELECT（写参数页）、
   RESERVE TRACK、WRITE(10)、SYNCHRONIZE CACHE、CLOSE TRACK/SESSION。CDB 布局与
   写参数页字段对齐 libburn 的同名实现（xorriso 的写后端），逐条有黄金断言。
2. `optiburn-engine` 增加 `NativeEngine`，接在 `BurnEngine` 同一个接缝上，名字
   `native`。写序列按当前 Profile 分组（分组照抄 libburn 在写参数页上的分支）：
   - CD-R / CD-RW：MODE SELECT 设 TAO 与数据轨 control，RESERVE TRACK，写，
     SYNCHRONIZE CACHE，关区段。
   - DVD-R / DVD-RW 顺序记录 / DVD-R DL：MODE SELECT 设增量写（BUFE、LS_V、固定包、
     轨道模式 5、link 与 packet size 16），RESERVE TRACK，写，关区段。
   - DVD+R[W]、DVD-RAM、BD-R[E]：不设写参数、不预留，写完按需关区段。
   - 其余 Profile（含受限覆盖 DVD-RW）拒绝并给出明确文案，不猜写序列。
3. 引擎选择按平台分：GUI 在 Windows 上走 `native`（那边没有可用的 xorriso），
   Linux 维持 `xorriso`（原生引擎还没在 Linux 真机验证过）；CLI 的 `--engine` 接受
   `xorriso` 与 `native`，默认仍是 `xorriso`。平台分支只有这一处，理由写在函数注释
   里：这是“哪条路走得通”的差异，不是设备语义差异。
4. 能力缺口做成结构化的 `NativeGap`（倍速、空镜像、容量、未知 Profile、可追加盘、
   已封口），CLI 与 GUI 各给母语文案，不复用 xorriso 的 stderr 分类。
5. Windows 的读盘仍是 xorriso（ADR-0012），本决策不动读侧；原生 ISO 9660 读取
   （hadris-iso 自带 read 模块，可复用）留待后续。

## 后果

- 写侧真机状态：完整序列（MODE SELECT、WRITE(10)、SYNCHRONIZE CACHE、
  CLOSE TRACK/SESSION）与写后读回对拍已在 CD-R 上通过（2026-10-10，见下节）。
  验证覆盖的是 CD-R 一族；DVD 各族只有替身传输层的端到端测试，等有对应介质再补。
- 可追加盘按 NWA 写新区段，覆写旧区段的策略仍在调用方的门禁里（CLI/GUI 对镜像写入
  一律拒绝可追加盘，ADR-0006/0010）。增长模式（合并既有区段）与倍速参数
  （SET CD SPEED/STREAMING，单位未核对）尚未支持，界面给明确文案而不是静默忽略。
- 写入被中止时盘上留下未完成的轨道，与 xorriso 被中止时相同，走同一套“已中止”文案；
  实测这种未关轨道的盘后续仍可继续写（NWA 跳过它），驱动器不计入区段数。
- MODE SELECT 的 multi 位承接“不封盘”默认，与 `--close-disc` 同义。
- 读侧分块上限（32 块）与 CD 容量预检的跳过原因是平台与介质事实，都写在代码注释里，
  将来传输层若暴露单次传输上限，可以把它从 mmc 挪到 transport。

## 被否决的方案

- 继续让 Windows 用户装 MSYS2 的 xorriso：实测不可用（无 MMC 传输层，ADR-0008 补记）。
- 捆绑链接 libcdio 的原生 xorriso 构建：要自行维护一条 GPL 二进制的构造与分发链，
  且只解决刻录、解决不了读盘，力气不如花在原生引擎上。
- CLI 也默认走 native：Linux 上原生引擎还没有真机验证，先不动已经验证过的路径。
- 按盘片状态自动挑引擎（有 xorriso 就用它）：Windows 上装了 MSYS2 的 xorriso 反而是
  陷阱（存在但不能访问光驱），按平台选是唯一诚实的规则。

## 补记：真机实测

- 2026-10-10 只读侦察（Windows，USB 光驱 D:，盘为 CD-R、可追加、5 个区段）：
  GET CONFIGURATION 报 Profile 0x0009（CD-R），READ CAPACITY 报 257819 个 2048 字节块，
  READ DISC INFORMATION 报可追加。三项与 `optiburn probe` 的口径一致。
- 2026-10-10 写盘实测（同一台机器与盘）：`NativeEngine` 把 81 块（165888 字节）的
  ISO 写到新区段（不封盘），依次走 MODE SELECT、WRITE(10)×3、SYNCHRONIZE CACHE、
  CLOSE TRACK/SESSION，随后用 READ(10) 把刚写的块读回，与镜像逐字节一致
  （`cargo test -p optiburn-engine -- --ignored native_burn_and_read_back_real`）。
  到达这一步的迭代翻出四个真机事实，都已落进代码注释：
  1. CD 上 RESERVE TRACK 被驱动器以 ILLEGAL REQUEST/INVALID FIELD IN CDB 拒绝，
     与 libburn 的写序列一致（它只在 DVD DAO 与 DVD+R SAO 上预留），写序列不再预留。
     命令留在 `optiburn-mmc` 里，将来做 DAO 时要用。
  2. CLOSE TRACK/SESSION 的 Close Function 是字节 2 的低三位（关区段 = 0b010），
     多移一位写成 0b100 同样被 INVALID FIELD IN CDB 拒绝。
  3. Windows 的 SPTI 单次传输卡在 64 KiB 量级：81 块的 READ(10) 被
     ERROR_INVALID_PARAMETER 拒绝，读侧按 32 块拆（写侧本来就是 32 块）。
  4. 可追加 CD-R 上 READ CAPACITY 报的是末区段卷空间（257819），与盘级写地址
     （NWA，实测 264720 起）不在同一地址空间，容量预检因此只对非 CD 介质生效。
- 系统侧的挂载视图要等换盘（弹出再放入）才刷新到新区段，这是多区段 CD 的常规
  行为，与写盘无关；字节级验证走上面的读回对拍。
