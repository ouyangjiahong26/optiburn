# ADR-0004：v0 用 xorriso 子进程刻录

- 状态：已接受
- 日期：2026-10-05

## 背景

镜像有了，接下来要写到盘上。可选：

1. 自己发 MMC 写入命令（`RESERVE TRACK` / `WRITE(10)` / `CLOSE TRACK`）——完整实现
   需要状态机、倍速协商、缓冲区欠载处理、盘片类型分支；
2. 调外部刻录程序（`xorriso`、`wodim`/`cdrkit`、`growisofs`）；
3. 系统 API（Windows IMAPI2）。

## 决策

v0 用 `xorriso -as cdrecord` 子进程，藏在 `BurnEngine` trait 后面：

```rust
pub trait BurnEngine {
    fn name(&self) -> &'static str;
    fn burn(&self, job: &BurnJob, progress: &mut dyn FnMut(f32)) -> Result<(), BurnError>;
}
```

参数为 `-as cdrecord dev=<设备> -data [speed=<N>] [-multi] <镜像>`。`-as cdrecord` 是
xorriso 文档化的 cdrecord 仿真层（本机 `xorriso -as cdrecord -help` 可查 `dev=`、
`speed=`、`-data`、`-multi` 均在其中）。GPL 边界止于进程边界：本仓库不链接 libburn、
libisofs 或任何 GPL 代码。

## 后果

- 立刻拿到一个能在 Linux 上真写盘的可用路径，代价只有一个运行时依赖。
- 缺依赖时的报错是可行动的：`spawn` 返回 `NotFound` 映射成
  `MissingTool("xorriso (sudo apt install xorriso)")`，其它 I/O 错误原样冒泡。
- 失败诊断从 stderr 尾部取摘要（最多 10 行），而不是把整个日志回灌给用户。
- **已知不足**：进度只是粗粒度提示。cdrecord 风格的输出里，缓冲区/fifo 的百分比与
  写入百分比同格式（`(fifo 100%) [buf 97%]`），v0 取行内第一个百分比，并在成功时统一
  补发 1.0。精确进度要等原生引擎（自己数 LBA）才有。
- 引擎可替换：`--engine` 已经按名字分派，非 `xorriso` 的值明确报“引擎 X 尚未实现”
  而不是静默忽略。

## 被否决的方案

- **v0 直接写原生 MMC 写入引擎**：写入状态机是这类工具中最容易出错的部分（倍速协商、
  写失败后的重试与同步、缓冲区欠载、驱动器忙时的轮询、CD/DVD/BD 与 +R/-R 分支）。
  本机与 CI 都没有光驱，写完无法验证——按“无验证不交付”的原则不放进 v0，留给 v0.5
  并在有硬件时逐步验证。
- **wodim / readcd（cdrkit）**：本机没有该工具；该项目的维护状态与跨平台可获得性都
  不如 xorriso。
- **Windows IMAPI2（`IDiscFormat2Data`）**：见 ADR-0005，它会把写 UDF Bridge 的决定权
  交给系统。
