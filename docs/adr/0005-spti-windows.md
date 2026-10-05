# ADR-0005：Windows 走手写 SPTI 绑定，不走 IMAPI2

- 状态：已接受
- 日期：2026-10-05

## 背景

Windows 上有两条访问光驱的路：

1. SPTI：`CreateFileW("\\.\E:")` 打开设备，用 `DeviceIoControl` +
   `IOCTL_SCSI_PASS_THROUGH_DIRECT` 发原始 CDB。
2. IMAPI2（`IDiscFormat2Data`）：COM 接口，把“怎么写盘”交给系统刻录栈。

## 决策

用 SPTI，手写绑定：`optiburn-transport/src/windows.rs` 只从 `windows-sys` 借
`CreateFileW`、`DeviceIoControl`、`CloseHandle` 与句柄类型。IOCTL 码与结构体在本地定义，
因为 `windows-sys` 并未导出这些符号。

本地定义的常量（值来自 `ntddscsi.h`，已核对）：

| 符号 | 值 | 说明 |
|---|---|---|
| `IOCTL_SCSI_PASS_THROUGH_DIRECT` | `0x0004D014` | `CTL_CODE(IOCTL_SCSI_BASE, 0x0405, METHOD_BUFFERED, FILE_READ_ACCESS\|FILE_WRITE_ACCESS)` |
| `SCSI_IOCTL_DATA_OUT` | `0` | 数据从主机发往设备 |
| `SCSI_IOCTL_DATA_IN` | `1` | 数据从设备发回主机 |
| `SCSI_IOCTL_DATA_UNSPECIFIED` | `2` | 无数据阶段 |

要点：

- `SCSI_PASS_THROUGH_DIRECT` 的 `Cdb` 是定长 16 字节，这决定了 trait 契约里
  CDB 上限就是 16。
- `TimeOutValue` 单位是秒（Linux 的 `sg_io_hdr.timeout` 是毫秒），转换写在模块内。
- 结构体布局断言为 56 字节，接上 32 字节 sense 缓冲后整个请求缓冲是 88 字节。
- `SCSI_PASS_THROUGH_DIRECT` 没有 residual 字段，`Completion.residual` 在 Windows 上
  恒为 0，这会让上层无法察觉短响应，已在 `optiburn-mmc` 的注释里标明。
- 设备路径要先规范成设备命名空间形式（`E:` 写作 `\\.\E:`）。

## 后果

- 传输层与 Linux 完全对称，`ScsiTransport` 是唯一需要平台分支的地方，未来写入引擎写
  一次即可在两端跑。
- x86_64 与 arm64 用同一套 API，交叉 `cargo check` 能覆盖（本仓库就是这样验证的）。
- 代价：Windows 分区大小 >2 TiB、驱动器出现在 `fsutil` 的卷 GUID 路径（`\\?\Volume{…}`）
  等边角情况要自己处理。`residual` 缺失使短响应只能靠上层的数据校验兜底。
- 现状：Windows 路径只验证过编译，没有真机与光驱可测。

## 被否决的方案

- IMAPI2 COM：它决定的是“刻一张 Windows 认为正确的盘”，而我们要的是“我们自己
  决定写 UDF Bridge 1.02/2.50”。文件系统与写策略的控制权会让渡给系统，`--profile`
  这类选项将失去意义。同时 COM 绑定与 arm64 支持都是额外负担，还要处理
  `IDiscFormat2Data` 的事件回调线程模型。
- 直接调 Windows 自带 `isoburn.exe`/`cdburn`：可用的参数面远小于 xorriso，且行为
  随 Windows 版本漂移，无法作为跨版本的可预期接口。
