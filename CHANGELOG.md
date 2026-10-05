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
