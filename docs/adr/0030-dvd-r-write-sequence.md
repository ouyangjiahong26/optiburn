# ADR-0030：DVD-R 写入序列对齐 libburn

- 状态：已接受
- 日期：2026-10-22

## 背景

原生引擎第一次在 DVD-R 上刻录时，写盘阶段成功，关区段被驱动器拒绝：

```
SCSI command [5b, 00, 02, ...] failed: status=2, sense=[70, 00, 05, ..., 72, 03, ...]
```

sense key 5、ASC 0x72、ASCQ 0x03，即 SESSION FIXATION ERROR / INCOMPLETE TRACK IN
SESSION，设备是 HL-DT-ST DVDRAM GP75N。同一张盘之前的另一个准备问题也一起暴露：
空白 DVD-R 上 `READ CAPACITY` 回末块 0，引擎据此判定“镜像超出介质容量”，而盘实际
有 4.4 GB。

CD 路径此前全部通过，说明问题只出在 DVD 这一档：写序列与参数只按 CD 验证过。

## 决策

以 libburn 1.5.6 的 DVD-R 增量写序列为准，逐条对齐（源码取自 Debian 的
libburn 1.5.4-1 包，1.5.6 与本仓库实测的 xorriso 1.5.6 同一代）。

1. 容量判据：`READ CAPACITY` 回 0 时改用 `READ FORMAT CAPACITIES` 的类型 1 描述符。
   空白 DVD-R 上 `READ CAPACITY` 的末块是 0（盘上还没有记录区），0 不是容量。
2. 写完补位：镜像块数向上补齐到整记录单位，DVD 是 16 块（32 KB）。libburn 的
   `obs_pad` 对 DVD-R 与 DVD+R 都置 1（write.c 的 profile 分支），在写最后一块
   缓冲时把不足 32 KB 的部分补零。
3. 关区段顺序：DVD-R 族与 BD 先 `CLOSE TRACK`（功能码 001b，轨道号取 READ DISC
   INFORMATION 字节 6）再 `CLOSE SESSION`（010b）。libburn 的
   `burn_dvd_write_track` 在每条轨道写完就调 `burn_disc_close_track_dvd_minus_r`，
   由 `burn_dvd_write_session` 在所有轨道之后调 `burn_disc_close_session_dvd_minus_r`。
   直接只发 `CLOSE SESSION` 时本机驱动器回上面那条 SESSION FIXATION ERROR。
4. CD 保持只发 `CLOSE SESSION`：TAO 的轨道由区段关闭隐式收尾，先关轨道会改变区段
   形状。

写参数页（Mode Page 5）本来就与 libburn 逐字节一致（BUFE 与 LS_V 置位、固定包、
轨道模式 5、link size 16、packet size 16），不需要改。libburn 给 `CLOSE TRACK/SESSION`
置 Immed 位再轮询，本仓库不置位、命令阻塞到驱动器完成，这个差异保留：两种都是
MMC 允许的形态，真机验证走的是阻塞形态。

## 后果

- 2026-10-22 真机验证（HL-DT-ST DVDRAM GP70N，空白 DVD-R，`--engine native
  --close-disc`，镜像 754 块补到 768 块）：写入与关闭都成功，盘片状态为已封口，
  `xorriso -indev` 列出全部文件，抽取回来两个文件的 sha256 与源一致，中文文件名
  完整，`salvage` 报“没有未关闭轨道”。
- 补位让写入块数比镜像多最多 15 块，容量门禁按补位后的块数比较。
- `PlusOrBdR` 也按 16 块补位：手头没有 BD 介质，BD 的簇是 32 块，这一档未经实测。
- GP75N（关区段被拒的那台）上的写入仍未复核，需要一张新的空白盘。同一张刻好的盘
  换到 GP75N 上做读侧复核已通过：盘片报已封口，系统自动按 UDF 挂载，两个文件
  sha256 与源一致，`probe` 与 `salvage` 都正常（这条路径依赖 ADR-0029 的
  `is_illegal_request` 容错：封口盘上 READ TRACK INFORMATION 问轨道 1 回 key 5
  加 ASC 0x21）。

## 被否决的方案

- 收到 SESSION FIXATION ERROR 再补发 `CLOSE TRACK` 后重试：把失败留给驱动器判断，
  且已经写坏过一次盘，不再走试错路径。
- 用 `-close_damaged` 的序列收尾正常写入：那是修复中断写入的路径，语义与正常收尾
  不同，libburn 也分成两个分支。
- 只靠驱动器自动补齐、不做用户态补位：实测轨道大小被驱动器凑到 768 块，但收尾
  仍失败过，补位与关轨道一起才对齐 libburn 的完整序列。
