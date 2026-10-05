# ADR-0002：镜像层用 hadris-cd（纯 Rust），不引 libisofs / genisoimage

- 状态：已接受
- 日期：2026-10-05

## 背景

「Windows 能读」这件事最终由镜像里的文件系统决定。需要在四个平台上生成 ISO 9660 +
Joliet + UDF Bridge 的镜像。候选：

1. hadris-cd（Rust，MIT，专做 UDF Bridge 混合镜像）；
2. libisofs FFI（C，libburnia 家族，GPLv2+）；
3. `genisoimage`/`mkisofs` 子进程。

## 决策

用 `hadris-cd = "2.5"`，包在 `optiburn-mastering` 后面，对外只暴露：

```rust
pub fn build_image(source_dir: &Path, output: &Path, spec: &ImageSpec)
    -> Result<ImageInfo, MasteringError>;
```

profile 到文件系统的映射写在一处（`options_for`），并在 `ImageInfo::filesystems` 里
回报实际写进去的内容。理由：

- MIT 许可，与本项目一致，没有链接传染问题；
- 纯 Rust，四个目标三元组都能 `cargo check`，不需要在 Windows/arm64 上准备 C 工具链；
- UDF Bridge 让 ISO 9660 与 UDF 共享同一份文件数据区，光盘容量不翻倍；
- 上游自带「用两套 reader 回读同一镜像」的测试，我们对它的信任有具体依据。

## 实现时必须知道的三件事（实测）

1. **输出文件必须以读写方式打开**。hadris 写完卷描述符后会回读并就地打补丁，
   `File::create`（只写句柄）会得到 `EBADF`，被上游折叠成 `Iso(Io(Source(Other)))`
   这种毫无信息量的错误。`build_image` 用 `OpenOptions::new().read(true).write(true)`。
2. **不需要 walkdir**。`FileTree::from_fs` 已经递归读取目录、按名字排序（保证同样输入
   产出字节相同的镜像）、跳过符号链接。计划里预留的 `walkdir` 依赖因此没有引入。
3. **UDF 卷识别序列不在扇区 17**。上游文档注释说「扇区 17–19 是 BEA01/NSR02/TEA01」，
   实测是 ISO 卷描述符序列占 16–19（PVD、Joliet SVD、1999 SVD、结束符），UDF VRS 在
   20–22。测试按标记搜索而不是写死扇区号，避免上游布局变化时测试假通过。

## 后果

- 镜像生成是无光驱也能完整验证的一步：`cargo test -p optiburn-mastering` 会真跑
  xorriso 解镜像并逐字节比对（含中文文件名、空文件、两级子目录、二进制内容）。
- 代价：可写的文件系统特性受上游限制（Rock Ridge 默认关闭、El Torito 未启用、
  没有 ISO 9660 之外的写入策略可选）。需要这些特性时优先给上游提 PR，而不是在
  本仓库 fork 一份或加一层自己的 ISO 写入器。
- `hadris-udf` 被直接依赖一次，只为命名 `UdfRevision::V2_50`（BD profile 需要）；
  版本必须与 `hadris-cd` 内部使用的一致，若不一致会在编译期因类型不同而报错。

## 被否决的方案

- **libisofs FFI**：GPLv2+（其 `COPYRIGHT` 明示），链接会改变本项目的分发许可；C 依赖
  还要在 Windows/arm64 上另建构建链。功能上它更强（Rock Ridge、El Torito、多区段），
  但 v0 不需要这些。
- **`genisoimage` 子进程**：本机（开发机与 CI 目标环境）都没有这个工具；它的 `-udf`
  支持落后于 `-J`，跨平台可获得性也差。用子进程生成镜像还会把「镜像字节一致」这件事
  交给外部工具版本决定，与 ADR-0003 的取舍冲突。
