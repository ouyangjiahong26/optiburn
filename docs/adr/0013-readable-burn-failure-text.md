# ADR-0013：写盘失败的文案按成因归类，原始输出进日志

- 状态：已接受
- 日期：2026-10-09

## 背景

实机拔线实验后，界面把 libburn 与 xorriso 的英文长文原样贴进结果横幅，里面混杂
WRITE(10) 的 CDB 十六进制、宿主机错误码和关闭区段的失败，普通读者无法据此判断
发生了什么、该怎么办。原始 tail 全量透传（ADR-0004 的摘要方式）对诊断有用，但
不该是面向用户的唯一说明。

## 决策

1. `optiburn-engine::BurnFailure::classify` 对失败输出做成因归类：目前覆盖
   `DriveLost`（Lost connection to drive、SG_ERR_DID_ERROR，实测拔线样本）与
   `DeviceBusy`（Cannot open busy device，实测挂载样本），其余归 `Other`。条目按
   真实遇到的报错逐步补充。
2. GUI 与 CLI 的失败文案按归类给出中文说明：讲清发生了什么、对盘的影响、下一步
   怎么做。只有 `Other` 保留原始输出。
3. 原始输出始终写入应用日志（GUI 进 stderr，落在会话日志文件里），诊断信息不丢。

## 后果

- 用户看到的是中文说明而不是 SCSI 报文。需要细节时查 `/tmp` 下的应用日志。
- 分类规则是字符串匹配，xorriso 换版本改写文案时可能失配，`Other` 兜底所以只会
  退化、不会误导。测试锁定了两个实机样本。
