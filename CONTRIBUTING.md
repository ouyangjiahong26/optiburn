# 参与开发

## 环境

- Rust 1.98.1（`rust-toolchain.toml` 已固定，rustup 会自动装）。
- `xorriso`（跑镜像回读测试与 `burn` 需要）：`sudo apt install xorriso`。
- 交叉编译目标：`rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
  x86_64-pc-windows-msvc aarch64-pc-windows-msvc`。

## 提交前必须跑的命令

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
for t in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
         x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
  cargo check --workspace --all-targets --target "$t"
done
npm ci --prefix frontend
npm run build --prefix frontend
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

CI 就跑这些（`lint` / `test` / `cross-check` / `gui` 四个 job），本地全绿再推送。

## 代码约定

- 中文注释，只解释“为什么”，不写“是什么”的复述。公开 API 必须有文档注释。
- 错误类型：每个 crate 一个 `thiserror` 枚举，`#[error]` 文案用英文。
  面向用户的文案（CLI 输出）用中文。
- 文件名/类型名：领域词汇一律取 `CONTEXT.md` 术语表，禁用同义词也在那张表里。
- 平台差异：只允许出现在 `optiburn-transport` 的 `linux.rs` / `windows.rs` 里，
  一层 trait 收口。新增平台分支前先看 ADR-0001。
- 不引 GPL 依赖：链接层面必须保持 MIT 可分发（ADR-0001/0002/0004）。子进程调用
  GPL 工具是可以的，链接不行。
- 不加 GUI 依赖：命令行优先。
- 测试：断言可观察行为、边界与错误路径，不断言实现细节与偶然默认值。硬件相关的
  测试必须能在没有硬件时明确跳过（`#[ignore]` 或打印 `SKIP`），并说明原因。

## AI 参与

本仓库的贡献可能由 AI 生成。约定：

- 标题加 `[AI Generated]` 前缀，例如 `[AI Generated][TASK] 初始导入`。
- PR 正文照下面的约定写，决策与取舍写进正文或 ADR，不要只在对话里说。
- AI 不得自行合并 PR（本仓库靠分支保护强制这一点）。

## PR 与 issue

- 每个 PR 至少关联一个同仓库 issue（纯文档小修除外）。用 `Fixes #NN` 或
  `Related to #NN`。
- 标题格式：`[意图标签] 一句话说明`，标签取 `[FEAT]`/`[FIX]`/`[DOC]`/`[TEST]`/
  `[CLEANUP]`/`[DEP]`/`[TASK]`。
- 议题标题另有一套词汇：`[BUG]`/`[FEAT]`（议题模板已预填），与 PR 的意图标签不通用。
- 走 PR、不直推 `main`：`main` 有分支保护，要求 `lint`/`test`/`cross-check`/`gui` 四个
  检查通过。

### 正文写作约定

正文写给不了解上下文的协作者，逐段回答下面五个问题：

1. 现状与影响：今天是什么行为，谁受影响，为什么值得改。
2. 方案取舍：为什么这样做，否决过哪些替代做法（和 ADR 矛盾的地方要显式指出）。
3. 改动脉络：按协作者读代码的顺序讲改了哪里。
4. 如何验证：跑了哪些命令、结果是什么。没法自动验证的部分写清手动步骤。
5. 从哪里继续读：关键文件与后续工作。

细节不要收进折叠区。不复述 diff。不写“方案 A/B”这类决策过程的痕迹（放进 ADR 或
commit 正文）。

## commit

- 一次提交一件事。信息按 `type: 摘要` 起头（`type` 取 `feat`/`fix`/`docs`/`test`/
  `refactor`/`chore`/`ci`）。
- 正文说明“为什么”，必要时注明关联 issue 与 ADR。

## 变更记录

用户可见的行为变化（新命令、新选项、输出格式、兼容性变化）写进 `CHANGELOG.md`
的 `Unreleased` 小节，格式照 Keep a Changelog。纯内部重构不写。
