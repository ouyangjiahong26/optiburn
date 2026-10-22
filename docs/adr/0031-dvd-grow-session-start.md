# ADR-0031：DVD 增长模式的区段起点口径

- 状态：已接受
- 日期：2026-10-22

## 背景

issue #63 记录了一处调研发现：KDE K3b 在 DVD 上除了从 TOC 取上一区段起点、从 NWA 取
下一区段起点之外，还对两个值各做一次修正，而本仓库的增长模式没有对应处理。

K3b 的取值在 `libk3b/projects/datacd/k3bdatamultisessionparameterjob.cpp` 的
`setupMultiSessionParameters()`，修正写在同一函数里：

```cpp
// pad to closest 32K boundary
nextSessionStart += 15; nextSessionStart /= 16; nextSessionStart *= 16;
// growisofs does it. I actually do not know yet why.... :(
lastSessionStart += 16;
```

修正之后的两个值作为 `-cdrecord-params "<上一区段起点>,<下一区段起点>"` 传给 mkisofs，
并用 `-prev-session <设备>` 让 mkisofs 去盘上读上一区段（`k3bisoimager.cpp` 的
`addMkisofsParameters()`）。K3b 自己不生成 ISO 布局，也不自己写盘。

## 上游依据

dvd+rw-tools 7.1 的 `growisofs_mmc.cpp`，`plusminus_r_C_parm()`（约 914 至 1004 行）：

- 顺序介质的下一区段起点取 `READ TRACK INFORMATION` 报的轨道起点，BD-R 的伪覆盖
  模式取 NWA
- 上一区段起点取 `READ TOC Format 1` 报的末区段起点
- 最后拼成 `"%d,%d"`，第一个参数是 `prev_session + 16`，第二个是 `next_session`

同一个文件的 `setup_C_parm()` 里，32 KB 对齐只出现在覆写介质与 poor man 分支
（`next_session += 15; /= 16; *= 16`），顺序介质走 `plusminus_r_C_parm`，不做补齐。
K3b 的补齐因此是防御性写法，不是 growisofs 顺序写路径的必需步骤。

`-C` 首值的语义在本机用一次离线实验确认过：同一份旧镜像 `a.iso` 与同一份新目录，用
`xorriso -as mkisofs -M a.iso -C <首值>,522` 生成增长区段。

| `-C` | 卷标 | 旧文件 | 新会话分配基址（根目录 extent） |
|---|---|---|---|
| `0,522` | EXPA（旧卷标） | 保留 | 541 |
| `202,522` | ISOIMAGE（默认值） | 丢失 | 541 |

分配基址由第二个值决定，与第一个值无关。第一个值只决定 mkisofs 去哪里读上一区段，
所以首值写错时的表现是旧树整体消失，而不是地址偏几块。

## 决策

1. 不引入 `prev_session + 16`。本仓库不生成 mkisofs 参数：旧区段由 `grow.rs` 自己读，
   读的位置是区段起点加 16 块的描述符区。把这个 16 加到区段起点上，PVD 就会读到
   区段起点加 32 块，直接读坏嫁接来源。上游那个 16 属于 mkisofs 的参数约定，不是
   盘上的地址约定。
2. 不引入 32 KB 对齐补齐。增长会话的布局基址与写入位置必须是同一个值，而写入位置
   由驱动器报的 NWA 决定，不能跳过块。补一个对齐值只会让布局基址与实际写盘位置
   分叉。growisofs 的顺序分支同样不补齐。
3. 保留 NWA 原样作为起点，并把这一点固定在单元测试里（`grow_start_uses_the_next_writable_address_verbatim`）。
4. DVD+R 或 DVD+RW 上的增长仍是待验证项。现有的 `native_grow_and_read_back_real`
   硬件测试补上起点与对齐的打印，介质到位后直接跑。

## 后果

- 增长路径的行为不变，CD 与 DVD 走同一套起点逻辑，尺寸预演与写入共用
  `grow_write_address`。
- 真机上若 NWA 不是 16 块对齐，本仓库按 NWA 原样写，与 growisofs 顺序分支一致。
  硬件测试会打印 `start % 16`，届时据此判断要不要另开 issue。
- 未覆盖：DVD+R 上的追加没有介质实测。介质前提是 DVD+R 或 DVD+RW，DVD-R 与
  DVD-RW 顺序记录写一次即挂起，做不出第二区段。

## 待验证

1. DVD+R 空白盘上跑 `OPTIBURN_DEVICE=<设备> cargo test -p optiburn-engine -- --ignored
   native_grow_and_read_back_real --nocapture`，确认旧路径全部保留、两个新文件逐字节一致。
2. 同一张盘拿到 Windows 7 上打开，确认资源管理器能列出合并后的全部文件。这一步与
   issue #61 的实验共用一张盘。
3. 记录真机 NWA 的 `start % 16`，核对“DVD 上 NWA 天然 16 块对齐”这个推断。

## 被否决的方案

- 照抄 K3b 的两处修正：`prev+16` 会读错旧区段，对齐补齐会让布局基址与写盘位置分叉，
  两处都不符合本仓库“自己生成会话、按 NWA 写”的模型。
- 改为调用 mkisofs 生成增长区段：这与 ADR-0020 的原生增长决策相反，Linux 上等于回到
  xorriso 子进程路线。
- 等拿到 DVD+R 再调研：调研本身不依赖介质，结论可以先落地，行为改动留到实测之后。
