# ADR-0001：自己写每平台的 SCSI 传输层，不引 libburn FFI

- 状态：已接受
- 日期：2026-10-05

## 背景

要往光驱发 MMC 命令，先得有一条能下发 CDB、取回状态与 sense 的通道。候选人选：

1. FFI 到 libburn / cdrskin（libburnia 家族，C 语言，Linux 上的事实标准之一）；
2. 自己写一层薄传输：Linux 用 `/dev/sr*` 上的 `SG_IO` ioctl，Windows 用
   `DeviceIoControl` + `IOCTL_SCSI_PASS_THROUGH_DIRECT`（SPTI）。

目标平台是 Linux 与 Windows 的 x86_64/arm64 四种组合。

## 决策

自己实现，落在一个 crate `optiburn-transport`，公开一个 trait：

```rust
pub trait ScsiTransport {
    fn issue(&mut self, cdb: &[u8], dir: Direction, data: &mut [u8], timeout: Duration)
        -> Result<Completion, TransportError>;
    fn device_path(&self) -> &str;
}
```

平台差异只存在于两个私有模块 `linux.rs`（`SG_IO`）与 `windows.rs`（SPTI），
`open()` 按 `cfg` 分发。trait 契约里写死：CDB 最多 16 字节（超长返回
`CdbTooLong`，不 panic）、sense 最多 32 字节、命令级成功只看 SCSI 状态字节与
宿主机/驱动状态是否全为 0；sense 不解释，原样交给上层。

## 后果

- 四个平台共用一条接口，`optiburn-mmc` 与未来的原生写入引擎不需要知道平台存在。
- 代价：两套 `unsafe` 绑定归我们维护。缓解手段是把两份 C 结构体的尺寸断言写成测试
  （`sg_io_hdr` 必须是 88 字节、`SCSI_PASS_THROUGH_DIRECT` 必须是 56 字节），布局漂移
  会直接测试失败而不是静默读写错字段。
- Windows 路径目前只保证编译通过（`cargo check --target *-pc-windows-msvc`），没有
  真机验证；Linux 路径也没有光驱可测，只验证了结构体布局与错误映射。
- 两套方向枚举语义相反（Linux `SG_DXFER_TO_DEV = -2`／`FROM_DEV = -3`，
  Windows `SCSI_IOCTL_DATA_OUT = 0`／`IN = 1`）这件事被关在模块内部，是这一层最容易
  写错的地方。

## 被否决的方案

- **libburn FFI**：官方 README 明确列出支持平台为“GNU/Linux（kernel ≥ 2.4）、
  FreeBSD（ATAPI/CAM）、OpenSolaris、NetBSD”——没有 Windows。即便绕过这一点，把
  GPLv2+ 的库链进 MIT 项目会把分发许可问题引入本项目，而我们需要它提供的只是
  `SG_IO`/SPTI 这层薄胶水（SCSI 命令本身的编解码仍在我们的 `optiburn-mmc` 里）。
  收益与代价不成比例。
- **抄 alight 的 `src/scsi/linux.rs`**：思路参考来源，但该仓库没有 LICENSE 文件，
  不能复制其代码；本仓库的 `linux.rs` 是按 `scsi/sg.h` 的 C 布局自行编写的。
- **走 `/dev/sg*` 而非 `/dev/sr*`**：`sg` 节点要额外枚举映射关系，而 `sr` 设备就是我们
  要操作的驱动器本身，少一层猜测。
