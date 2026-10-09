# Issue tracker: GitHub

本仓库的 issue 和任务要求存放在 GitHub Issues 中。所有操作使用 `gh` CLI。

## 约定

- 创建 issue：`gh issue create --title "..." --body "..."`。多行正文用 heredoc。
- AI 贡献标记：AI 提交的 issue 标题以 `[AI Generated][<类型>]` 开头（类型是大写标签，如 `[FIX]`、`[DOCS]`）。PR 标题即 squash 合并后的提交主题，用传统提交格式 `type(scope): 摘要`，不加 AI 前缀。AI 写的评论首行用 `> **[AI Generated]** 本评论由 AI 完成。` 或 `> **[AI Assisted]** 本评论由 AI 辅助完成。`
- 读取 issue：`gh issue view <number> --comments`，用 `jq` 过滤评论，同时获取标签。
- 列出 issue：`gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'`，按需加 `--label` 和 `--state` 过滤。
- 评论 issue：`gh issue comment <number> --body "..."`
- 添加 / 移除标签：`gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- 关闭：`gh issue close <number> --comment "..."`

从 `git remote -v` 推导仓库，`gh` 在 clone 内运行时自动识别。

## issue / PR / 评论的格式

文字质量由注入 `AGENTS.md` 的写作要求管，本节管结构与流程。分类、标签与推进面板见下文各节，不重复。

### 提案先行

- 动手写代码之前先发 issue 提案，末尾写明是否已有可用实现，得到维护者回应再动手。错别字、修法唯一的明显 bug 等小改动可以直接提 PR。
- 发 issue 前先搜：文档、README、既有 issue 与 PR。已有答案或正在进行的，评论到既有帖子里，不发新帖。

### issue

- 标题说清对象和目的，不写方案。
- 正文三段：Problem 讲现状与问题，用文档、代码行、真实实例作证据，不写抽象抱怨。Proposal 讲打算怎么改，具体到可判断（关键参数与默认值、保守用户退回现状的开关、为什么安全），有多个选项时给推荐和理由。上下文 列出相关的前序 issue、commit、ADR。
- 需要维护者决策的点单独列出（待决策），没写就视为没有。后续在 PR 里落地的，合并前回 issue 评论决策结果。决策记在 issue，不记在 PR 描述里。

### PR

- 标题回应 issue 标题：解决哪个 issue，就用它的词说同一件事，不按实现手段另起名字。
- Closes 置顶，其后五段：Summary 一段话讲做了什么。Motivation 为什么值得做，呼应 issue 的 Problem。Changes 逐个文件讲改动点与动机。Why this is safe 说明不变量未动、退路与边界情况。Test plan 逐项报结果，只报事实（命令、数字、截图），既存失败标注 pre-existing。
- 与 issue 方案不一致的落法单独交代。对不上账的，`Closes` 改 `Refs`。

### 评论

- 按身份说话：维护者评审外部 PR 时给结论（我认可 / 需要改 / 此条撤回），不用商量式的口气，也不替对方解释动机。
- 先立主线再展开：先说这条 issue / PR 在解决什么问题，文件职责与流程对账是支撑。修了什么，先讲因果再讲改法。
- 收尾简短闭环。靠分析说服，不靠篇幅。不用引号包裹术语，不用破折号，不用装饰性符号。
- 不代改他人提交的内容：别人的 issue 正文、PR 描述、评论有问题，以评论提出请作者自己改。维护者直接改的只有 PR 标题、标签、里程碑这类元数据。
- AI 生成的 issue、PR 与评论按“约定”一节的标记规则标识。

## Pull request 作为分诊渠道

PR 作为请求渠道：否。

GitHub 的 issue 和 PR 共享编号空间，所以 `#42` 可能是其中任一，用 `gh pr view 42` 确认，回退到 `gh issue view 42`。

## 当技能说“发布到 issue tracker”时

创建一个 GitHub issue。

## 当技能说“获取相关工单”时

运行 `gh issue view <number> --comments`。

## GitHub Project

使用 Project：是。`/github-project`、`/triage`、`/open-pr`、`/merge-pr` 读取此配置。

| 项 | 值 |
| --- | --- |
| Owner | `ouyangjiahong26` |
| Project 编号 | `11` |
| Project ID | `PVT_kwHOCpw4xM4BlyI4` |
| Status 字段 ID | `PVTSSF_lAHOCpw4xM4BlyI4zhkdmeM` |

Status 选项 ID：

| 选项 | 选项 ID |
| --- | --- |
| `Todo` | `f75ad846` |
| `In Progress` | `47fc9ee4` |
| `Done` | `98236657` |

`Priority`（`P0`–`P3`）、`Start Date` 等其他字段按同样格式记录字段 ID 与选项 ID。没记录的字段，技能不写。状态迁移规则见 `/github-project`。

注：分诊流程期望的状态 Inbox、Backlog、Ready、In review、No action 在此 Project 中不存在。GitHub API 不支持给已有单选字段追加选项，需要时在 Project 网页界面的 Status 字段设置里添加，再回填本表。
