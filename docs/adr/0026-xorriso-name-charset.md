# ADR-0026：xorriso 调用的名字侧字符集固定为 UTF-8

- 状态：已接受
- 日期：2026-10-15

## 背景

ADR-0024 起读侧为了解析英文关键行固定 `LC_ALL=C`，代价写在那条的补记里：xorriso
在该 locale 下把 Joliet 的非 ASCII 名换成替代名（libisofs 报 `Cannot convert from
charset UTF-16BE to ANSI_X3.4-1968`）。当时的定位是“xorriso 优先这一档的既有代价”。

ADR-0028 的追加真机验证暴露了同一根因在写侧的后果，比读侧严重：增长运行要读出盘上
旧区段的名字再写进新会话，而 xorriso 的本地字符集来自调用方的 locale。非 UTF-8 环境
（`LC_ALL=C`、shell 里没有生成 UTF-8 locale、服务或自动化环境）里跑一次追加，
libisofs 会把旧区段每个非 ASCII 名换成带哈希的替代名并写进新会话。本机离线复现
（xorriso 1.5.6，镜像文件当盘）：

```
盘上原有   /默认档测试.txt
LC_ALL=C 增长后   /____hcm____._t_x_t137714629b44773be78118882a756025
```

盘是一次写介质，写坏的名字就是永久结果，Windows 资源管理器按 Joliet 显示，用户看到
的就是替代名。真机那张盘的第 3 个区段正是这么来的：第 2 个区段之后末区段的 3 个中文
名已是替代名，此后的写入只是把它继续带下去。文件名与大小都在，数据不受影响。

## 决策

1. 所有会碰文件名的 xorriso 调用在参数表最前面加 `-local_charset UTF-8`，把名字侧
   字符集与消息侧 locale 解耦（`xorriso.rs` 的 `local_charset_args`）：
   - 写侧：`grow_base_args`，覆盖 `-print_size` 预演与 `-commit` 提交两条同源的
     参数表。
   - 读侧：`pvd_info_args`、`list_args`、`extract_args`、`extract_paths_args`，即
     卷标、列目录树、整树抽取、按路径抽取。
2. 不加的地方与理由：镜像刻录（`-as cdrecord`）不碰盘上名字，只把镜像文件路径原样
   交给 libburn。`repair` 与 `-toc` 只读结构、不列名字。改动面保持在“名字会进出
   设备”的那几条。
3. 消息侧仍固定 `LC_ALL=C`（ADR-0024 决策 1 的解析前提），不动。

## 验证

- 离线复现（本机，xorriso 1.5.6，镜像文件当盘）：带 `-local_charset UTF-8` 时旧区段
  的 `/默认档测试.txt` 与新文件的 `/原生档测试.txt` 都原样留在新区段，两个命名空间
  都是真名。不带时旧名成替代名、新名保留，与真机上看到的形态一致。
- 回归测试 `xorriso::tests::grow_keeps_joliet_names_under_a_c_locale`：用模块自己的
  参数表跑一次真增长（`stdio:` 伪设备、`LC_ALL=C`），读回镜像断言两个名字都在。
  去掉 `-local_charset UTF-8` 时该测试失败，加上后通过。
- 真机侧不再写盘确认：已经在盘上的替代名无法修回（目录记录已落在一次写介质上），
  要恢复只能按原名重新追加这些文件。

## 后果

- 非 UTF-8 环境里的追加不再写坏盘上原有的非 ASCII 名。ADR-0024 补记里“xorriso 读
  Joliet 非 ASCII 名给替代名”的既有代价随之消失，Linux 上 xorriso 优先这一档的
  读盘名字与原生读侧一致。
- 已经写坏的名字不回修，ADR-0028 的真机验证记录里对此有说明。
- 文档同步：ADR-0024 补记加一行指向本 ADR，`CHANGELOG.md` 记修复。
