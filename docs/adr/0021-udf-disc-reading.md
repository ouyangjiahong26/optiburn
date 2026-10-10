# ADR-0021：UDF 盘读侧

- 状态：已接受
- 日期：2026-10-10

## 背景

ADR-0018 的原生读侧只认 ISO 9660：盘上最后一区段没有 `CD001` 的 PVD 就报
`NoIsoSession`。Windows 写入的纯 UDF 盘与多区段 UDF 盘的末区段都落在这条路上，
于是 Windows 上设备页浏览、复制、回读校验对这类盘全不可用，报“末区段不是 ISO
9660”。ADR-0018 把这条记为缺口，注释里也明确“Windows 写的纯 UDF 盘仍报
NoIsoSession”。

盘上这类盘不少（Windows 原生刻录默认写 UDF）。`hadris-udf` 2.5（MIT，同一上游）
提供读侧：卷识别序列解析、锚点定位、卷描述符序列、分区、文件集、目录与文件读取。
缺的只是一层把末区段按正确地址喂给它的适配器。

## 决策

1. 读侧新增 `disc_read/udf.rs`：`UdfSource` 实现 std 的 `Read` 与 `Seek`，按探测
   出的地址约定把 hadris-udf 请求的块号映射到盘上物理块，再用 `UdfVolume::open`
   解析。`NativeRead` 的四个读函数按 `DiscSession::{Iso, Udf}` 分支；`open_session`
   先按 ISO 9660 打开，只有报 `NoIsoSession` 时才试 UDF。Bridge 盘因此仍走 ISO
   分支，与追加门禁的语义一致（`last_session_is_iso` 不看 UDF）。
2. UDF 的发现位置按区段内固定偏移定位，内容地址有两种约定，探测自锚点的 tag：
   - `BEA01` 与 `NSR02`/`NSR03` 的卷识别序列在“区段起点加 16”起的 16 块里
     （内核 `fs/udf/super.c` 的 VRS 扫描同一位置）。
   - 锚点（AVDP）探测读物理块“区段起点加 256”（读不到或不是 AVDP 时再试加 512）。
     依据是内核只在这三处用区段起点：VRS 扫描、AVDP 探测、兜底。
   - 锚点 tag 的 tag_location 字段决定约定：等于 256 表示内容地址是区段相对
     （自家原生引擎把独立镜像原样写进区段的形态）；等于区段起点加 256 表示内容
     地址是盘级绝对（标准写入器的多区段盘，内核 `udf_read_ptagged` 把内容地址当
     物理块用）。起点为 0 时两种约定重合，按单段处理。
3. `DiscAbsolute` 下做一处归一化：请求块落在固定发现位置（区段内偏移 16 到 31、
   256、512）且读到的是 AVDP 时，把缓冲里的 tag_location 改写成请求块号并重算
   tag 校验和（字节 4 是字节 0 到 3 与 5 到 15 之和），因为 hadris-udf 只按“块
   256”找锚点，并用请求块号校验 tag_location。描述符内容里的地址一律不动：那些
   本来就是绝对地址。改过内容的描述符同时重算 descriptor CRC。
4. 位移夹具与真实盘不一致的地方要写清楚：只有**卷结构描述符**（tag 标识 1 到 9，
   位于分区之外）的 tag_location 是物理块号，可以位移；分区内的描述符（文件集
   描述符、文件入口 ICB、文件标识描述符）的 tag_location 是分区内逻辑块号，内核
   与 hadris-udf 都按它校验，位移它们会立刻校验失败。ADR-0018 的“盘上内容地址
   一律绝对”只适用于卷结构层，不要推广到分区内。
5. 错误口径：
   - 区段里没有卷识别序列（空白盘、音频盘、纯 ISO 盘）报 `NoIsoSession`，与 ISO
     侧同一道门（设备路径上先查 `READ DISC INFORMATION` 的盘片状态，空盘直接归
     这里）。
   - 有卷识别序列但固定位置没有锚点、或 tag_location 两种约定都对不上，报
     `UnsupportedUdf`（VAT、包写盘这类把锚点放在别处的结构）。
   - 有卷识别序列、锚点也认得出，但 `UdfVolume::open` 失败（元数据分区、扩展
     分配描述符、不认识的卷描述符序列）报 `UnsupportedUdf`，附上游错误文案。
     这一类不能退回 `NoIsoSession`：盘上确实有 UDF，说“没有 ISO 9660”会误导。
6. 末区段起点、路径安全与取消沿用 ISO 侧的规则：设备路径取 READ TOC Format 1 的
   末区段起始地址（取不到按 0），镜像文件起点 0；落盘名与路径逐段过
   `safe_relative_path`；目录递归加深 64 层上限拦住损坏镜像里的环；每个条目检查
   取消令牌。设备路径的源长度在打开时未知，给一个小于 `i64::MAX` 的哨兵值
   （hadris 读文件前会 `seek(End(0))`，取 `u64::MAX` 转成 i64 会变负数）。
7. 卷标取 UDF 主卷描述符的卷标识（去尾部 NUL 与空格），空值与 ISO 侧同口径报
   “读不到卷标”。`last_session_is_iso` 的语义不变：它回答的是“末区段是不是 ISO
   9660”，UDF 归“不是”，追加门禁因此照旧拦住 Windows 写的 UDF 盘。

## 后果

- Windows 上设备页浏览、整树抽取、按路径抽取、卷标读取对纯 UDF 盘与多区段 UDF 盘
  的末区段都可用；Bridge 盘仍按 ISO 9660 读，名字走 Joliet。
- 只影响读侧：不改变任何写盘行为，UDF 盘仍然不能续写（追加门禁不变，见 ADR-0010
  与 ADR-0020）。
- `hadris-udf` 进依赖树（MIT，默认 feature 含 std/sync/read），`hadris-cd` 只作为
  engine 的 dev-dependency 用来造 UDF-only 测试镜像。
- 验证范围与未实测项：
  - 单元测试七组：`hadris-cd` 的 `udf_only()` 镜像（UDF 1.02，卷识别序列 16 到 18、
    锚点 256、主卷描述符序列 257 起、分区 290）在起点 0 下读卷标、列举、抽取与中文
    名逐字节对拍；`build_image` 的 Bridge 镜像必须走 ISO 分支，同时证明它的 UDF 侧
    也能被打开（iso-first 是策略不是能力）；同一 UDF 镜像放在合成盘起点 1000 处按
    区段相对约定读通；同一镜像位移成盘级绝对形态（卷结构描述符的 tag_location、
    锚点的两个 extent、分区的 `partitionStartingLocation`、逻辑卷描述符的完整性
    extent、未分配空间描述符的 extent 都加位移并重算 tag 校验和与 CRC）后放在起点
    4000 处读通；没有卷识别序列报 `NoIsoSession`；卷识别序列在但锚点被清零报
    `UnsupportedUdf`。
  - 没有实物 UDF 盘可用（本仓库的测试盘是 CD-R，Windows 写的纯 UDF 盘样本不在
    手上），也没有 Linux 主机做 `mount -t udf` 的独立对拍。因此“Windows 刻的纯
    UDF 盘”只按内核语义构造的夹具覆盖，未在实物上验证。风险集中在两点：标准写入器
    是否真的把内容地址写成物理块（内核源码与多区段 xorriso 区段的实测支持这一条，
    但 UDF 侧没有实物证据）、以及锚点在 256 与 512 之外的介质（VAT、包写）会落
    `UnsupportedUdf`，那时需要新的夹具或实物盘。
  - `DiscAbsolute` 的映射假定区段起点大于 512（固定发现位置与内容地址不撞车）。
    真实盘的区段起点远大于这个值，夹具用的是 1000 与 4000。

## 补记：独立工具对拍（2026-10-11，Ubuntu 主机 + 内核 udf 驱动 + udftools + xorriso）

读侧本身的验证只有自造镜像时说服力有限，因此用独立实现在同一批夹具上复核：

- Bridge 镜像（`build-image --profile dvd`，本仓库母盘制作的产物）：`udfinfo` 报
  `label=UDFTEST`、`udfrev=1.02`、卷识别序列从块 16 起共 7 块（16 到 19 是 ISO
  描述符，20 到 22 是 `BEA01`/`NSR02`/`TEA01`）、锚点 256、主卷描述符序列 257 起、
  `integrity=closed`；内核 `mount -t udf` 列出 UDF 侧的同一棵树（中文名录得对），
  内核 `mount -t iso9660` 列出 ISO 侧同一棵树，两侧的文件内容与我们本地的源文件
  md5 一致。这说明 Bridge 盘两个命名空间都标准，也说明 iso-first 是策略而不是
  被迫（UDF 侧也能读）。
- UDF-only 夹具：`udfinfo` 报 `label=UDFONLY`、`udfrev=1.02`、卷识别序列块 16 起、
  锚点 256、主卷描述符序列 257 起；内核 `mount -t udf` 列出的路径集合与我们的
  `list_tree` 完全一致，三个文件的 md5（含跨扇区的 5000 字节二进制文件）与我们
  `extract_tree` 抽出的字节逐个相等（`bcfd6d08…`、`d75d4e71…`、`046b3239…`）。
- 上游怪癖（写在这里避免下一个人重踩）：`hadris-cd` 的 `udf_only()` 把卷识别
  序列写在逻辑块 19（沿用了 Bridge 的描述符区偏移），而 ECMA-167 要求从块 16 起。
  `udfinfo` 与内核 udf 驱动都拒绝块 19 的布局（`UDF Volume Recognition Sequence
  not found`），只有 hadris-udf 的读侧（扫 16 到 31）能读。测试夹具因此把这三块
  搬到块 16 再断言（见 `udf/tests.rs` 的 `relocate_vrs`），读侧保持宽容：两种位置
  都认，实物盘（标准写入器）的块 16 与这种历史产物都能读。



## 被否决的方案

- 自己写 UDF 解析器：卷描述符序列、分区、ICB、分配描述符、文件标识描述符与
  Unicode 名字解码是一整套规范，`hadris-udf` 已经在依赖树里且能读 `udf_only()`
  产物，自写只有维护成本。
- 在 ISO 读失败时一律改报 UDF 错误：纯 ISO 盘、空白盘、音频盘都没有卷识别序列，
  报 `UnsupportedUdf` 会让“空白盘”这类正常情形变成故障。按卷识别序列的有无分流。
- 把 UDF 也接进 `last_session_is_iso`：那个门禁的语义是“末区段是不是 ISO 9660”，
  追加 ISO 区段会遮住 UDF 盘原有内容的判断因此成立。改它等于放弃这条保护。
