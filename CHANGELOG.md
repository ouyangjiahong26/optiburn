# Changelog

本项目的所有显著变更都记录在本文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本管理遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased] - 初始设计与骨架

### 新增

- 5-crate Cargo workspace 骨架：`optiburn-transport`（SG_IO/SPTI SCSI 传输）、
  `optiburn-mmc`（MMC 读侧命令）、`optiburn-mastering`（hadris-cd ISO9660/Joliet/UDF
  镜像生成）、`optiburn-engine`（xorriso 子进程刻录引擎）、`optiburn-cli`。
- 软件设计文档：架构分层、Windows 兼容性矩阵、5 份 ADR、术语表（`docs/`）。
- 治理：CI（lint/test/cross-check）、发布工作流、分支保护、安全报告、议题与 PR 模板。

### 变更

- probe 的设备枚举移到 `optiburn_transport::list_optical_devices`，非 Linux 平台从报
  “probe 尚未支持该平台”改为报“未发现光驱”，退出码同为 0。
- CLI 中文文案的冒号与括号改为全角（“错误：”“打开失败：”“其它（3）”等）。

### 修复

- CLI 顶层帮助文案改为中文，与本仓库“面向用户的文案用中文”的约定一致。
- `ImageInfo.sectors` 只在字节数是 2048 整数倍时给出，否则返回 `MisalignedImage`。
  不再用向上取整推算一个无法核实的扇区数。
- 刻录引擎把镜像路径按 `OsStr` 原样传给 xorriso，非 UTF-8 路径不再被替换成 U+FFFD。
- 平台相关修正：Windows 目标不再产生未使用导入与死代码警告（cross-check 现在以
  `-D warnings` 跑）。其它平台的传输层测试不再假定 `open()` 必然返回 `NotFound`。
- 文档纠正三处不实声明：镜像“字节跨次一致”（实际含构建时刻时间戳）、引擎区分
  `unsupported`、以及 `READ DISC INFORMATION` 字节 3 的语义（盘上首轨号）。
- 文档与注释统一为弯引号，术语改用 `CONTEXT.md` 术语表词汇（盘片状态／区段／设备路径／
  UDF Bridge）。
