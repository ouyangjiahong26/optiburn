# ADR-0007：Linux 打开光驱必须 O_NONBLOCK

- 状态：已接受
- 日期：2026-10-08

## 背景

本机（ThinkPad Ultraslim DVD USB 光驱，空白 CD-R）实测：`open("/dev/sr0", O_RDWR)`
返回 `EROFS`（Read-only file system），同一时刻 `O_RDWR | O_NONBLOCK` 能打开。内核在
非阻塞打开光驱块设备时会跳过同步介质检查；走阻塞路径时空白盘在写打开下被判成只读
设备。cdrecord、sg_utils 等同类工具也以 `O_NONBLOCK` 打开 `/dev/sr*`。光驱刚插入、
盘片识别未完成时稳定复现，识别完成后偶尔能过，属于时序相关的怪癖。

## 决策

`optiburn-transport` 的 `LinuxSg::open` 一律带 `O_NONBLOCK`
（`OpenOptions::custom_flags`）。`SG_IO` 走 ioctl，不受该标志影响；介质是否就绪由
`mmc` 的 `TEST UNIT READY` 显式回答（ADR-0006 的前置检查），不依赖 open 的副作用。

## 后果

- probe 与刻录不再因打开时序失败。
- 打开本身不再等待盘片旋转，慢盘的等待集中在前置检查，超时语义只有一处。
- Windows 的 SPTI 打开路径不受影响（`CreateFile` 没有 O_NONBLOCK 语义）。
