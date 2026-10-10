# Windows 可读性

目标读者是“用 optiburn 做盘、在 Windows 上打开”的人。这里记录每种介质写哪些文件系统、
为什么，以及哪些行为是实测过的、哪些是从标准推出来的。

## 介质 × 文件系统

| Profile | ISO 9660 | Joliet | UDF | 依据 |
|---|---|---|---|---|
| `cd` | Level 2 + 长文件名（ISO 9660:1999） | Level 3 | 不写 | Windows 从 XP 起原生读 Joliet。CD 容量小，UDF 收益有限 |
| `dvd`（默认） | Level 2 + 长文件名 | Level 3 | 1.02（UDF Bridge） | DVD-ROM 通用标准。Windows Vista+ 认 UDF 1.02 |
| `bd` | Level 2 + 长文件名 | Level 3 | 2.50 | 蓝光标准要求。BD 播放/读取设备普遍只认 2.50+ |

三个 profile 都保留 ISO 9660 主命名空间：老系统（DOS、老式机顶盒、部分车载机）只能读
ISO 9660，丢了它盘就成废盘。Joliet 提供 Windows 的长文件名与中文名，UDF 提供现代读取
路径。ISO 与 UDF 共享同一份文件数据区（UDF Bridge），不占双倍空间。

### 依据链

- UDF Bridge 布局：hadris-cd 同时写 ISO 9660 与 UDF 元数据并让两者共享同一份文件
  数据区，上游自带“用两套 reader 回读同一镜像”的测试。本仓库再用 xorriso 独立回读一次
  （`crates/optiburn-mastering/tests/roundtrip.rs`），把这条依据钉在可执行的检查上。
- 命名空间组合：与既有的 `genisoimage -r -J -udf -iso-level 3` 口径一致：长文件名
  对应本仓库的 ISO 9660:1999 长文件名开关，`-J` 对应 Joliet，`-udf` 对应 UDF。`-r`
  （Rock Ridge）本仓库 v0 未启用。该参数组合是计划记录的既有实践，对应工具不在本仓库，
  只作口径对照，不是可执行验证。
- 介质与 UDF 版本：DVD-ROM 通用标准用 UDF 1.02。蓝光要求 UDF 2.50 及以上。

## 镜像里实际长什么样（实测）

`build-image --profile dvd` 产出的镜像，用字节偏移直接读出来：

| 扇区 | 内容 |
|---|---|
| 0–15 | 系统区（本版本全 0，El Torito/混合启动未启用） |
| 16 | ISO 9660 主卷描述符（类型 1，`CD001`） |
| 17 | 补充卷描述符（类型 2，Joliet） |
| 18 | 补充卷描述符（类型 2，ISO 9660:1999 长文件名） |
| 19 | 卷描述符序列结束符（类型 255） |
| 20–22 | UDF 卷识别序列：`BEA01` / `NSR02` / `TEA01` |
| 256 | UDF Anchor Volume Descriptor Pointer |
| 其余 | 目录结构与文件数据 |

`NSR02` 表示 UDF 1.02，`NSR03` 表示 UDF 2.00 及以上。`--profile bd` 写的是 `NSR03`，
`--profile cd` 完全没有 UDF 那三个扇区。这些断言在 `crates/optiburn-mastering/src/lib.rs`
的测试里锁住了，防止以后换上游版本时布局悄悄变掉。

> 注意：上游 hadris-cd 的文档注释写“扇区 17–19 是 UDF 卷识别序列”，与实测不符：
> 它的 ISO 卷描述符序列实际占 16–19，UDF VRS 在 20–22。本文件以实测为准。

## 怎么验证盘在 Windows 上能读

1. `optiburn build-image <目录> -o disc.iso --profile dvd`。
2. `xorriso -indev disc.iso -ls /` 确认参考实现能列出全部文件（`cargo test -p
   optiburn-mastering` 会自动做这件事，并逐字节比对内容，含中文文件名）。
3. 刻录后插到 Windows：资源管理器应当直接挂载并显示卷标。`fsutil fsinfo volumeinfo X:`
   会报出实际使用的文件系统（UDF 与 Joliet 的优先级由 Windows 决定）。
4. 中文文件名、空文件、两级以上子目录、二进制内容是必须复核的四项。

实测范围说明：第 1、2 步以及镜像结构断言在 Linux 上真实执行。第 3 步（在 Windows 上把
镜像刻到盘再读回）已于 2026-10-10 在一台 Windows 机器上用 USB 光驱与一张 CD-R 做过，
走的是原生 MMC 引擎（见 ADR-0017 补记）。

## 多区段（multi-session）

| 介质 | 能否追加区段 | 说明 |
|---|---|---|
| CD-R / CD-RW | 可以 | 每次追加占用 lead-in/lead-out，约 20 MB 开销 |
| DVD+R / DVD+RW | 可以 | 结构上原生支持 |
| DVD-R / DVD-RW | 不可以 | 未封口即为挂起状态，追加会失败 |
| DVD-RAM | 可以（按 UDF 包写） | 行为像可移动磁盘 |
| BD-R / BD-RE | 可以 | SRM/伪覆盖两种模式 |

Windows 只挂载最后一个区段，所以追加刻录用增长模式：`optiburn append`（GUI 是追加页）
读出已有区段的目录树，把源目录并入后作为新区段提交，最后一个区段携带全部文件，资源
管理器看到的始终是合并视图。以独立镜像方式往可追加盘上写（`burn`）会把旧文件遮住，前置
检查会拒绝这条路并提示改用 `append`。刻录默认不封盘，`--close-disc`（`burn` 与
`append` 都支持）写完把盘标记为不可追加；对 DVD-RAM、BD-RE 这类随机可写介质，
xorriso 的 `-close` 不生效，这类盘无需封盘即可覆写。

追加在 Windows 上走原生引擎（ADR-0020），不再依赖 xorriso：它会读出末区段的
Joliet 目录树，生成一个只含目录结构与新文件数据的新区段，旧文件的数据块原地引用。
这个新区段是 ISO 9660 加 Joliet，不带 UDF 描述符（ADR-0020 决策 3）。追加后的盘在
Windows 上仍读得出全部文件，卷标是新区段的卷标。资源管理器要弹出光盘再放回才会刷新
挂载视图（Windows 只在换盘时重读区段表）。旧区段形状不支持嫁接时（没有 Joliet、
含启动记录、含多 extent 文件等）追加会被拒绝并说明原因，换空白盘重刻即可。

末区段不是 ISO 9660 的盘（例如 Windows 写入的 UDF 盘）不能按增长模式续写：追加
ISO 区段后，Windows 只看最后一个区段，原有文件会从资源管理器里消失。`append`
的门禁会检测并拒绝这种盘，提示换空白盘重刻。这类 UDF 盘的内容本身可以浏览与复制
（原生读侧能读 UDF，见 ADR-0021），只是不能续写。

刻录中断（拔线、断电）留下坏末区段时，情况分两种。驱动器把未完成的轨道排除在 TOC
之外的形态本来就能用：区段数不变、末区段起点仍是上一个完整会话，读盘与追加都正常
（2026-10-11 实测复现）。固件把残片登记成末区段时，Windows 侧会回退到最新的可用
区段（ADR-0022）：读盘照常列出完好区段里的文件，追加把新会话嫁接在那个区段上并要
求用户确认，因为被跳过区段里的文件会从资源管理器的可见目录里消失。Linux 的 xorriso
路径没有这条回退，这类盘在那边仍报末区段不可读。

制作端在 Linux 上有两条要留意的事实，均在本机实测（CD-R，HL-DT-ST GP70N）。

- 内核把多区段盘的块设备容量停在第一区段，`blockdev --getsize64` 只覆盖第一区段
  的数据，桌面自动挂载看到的是第一区段的内容，追加进后续区段的文件在文件管理器
  里不可见。要看全部内容用 `xorriso -indev <设备> -ls /`，或用 GUI 的回读校验。
- 光盘被桌面挂载期间写盘引擎拿不到独占访问（libburn 报设备占用），必须先卸载。
  CLI 与 GUI 的写前检查会拦下并给出卸载命令，见 ADR-0010。
