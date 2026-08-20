# Figstash Agent 协作规范

本文件适用于 Figstash 仓库中的全部目录和任务。所有 Agent 在分析、设计、实现、测试、
文档维护和交付时都必须遵守以下约定。

## 1. 修改授权

- 在用户明确要求实施或修改之前，不得直接修改代码、配置、文档或 Git 状态。
- 讨论方案、解释行为、诊断问题、审查代码或提出建议，不等同于获得实施授权。
- 未获得实施授权时，可以执行必要的只读检查，但必须保持工作区不变。
- 用户只授权某类文件或某个范围时，不得顺带修改范围外内容。
- 不得因为某项修改看起来合理、简单或属于自然后续步骤，就推断用户已经授权。

## 2. 权威文档与更新顺序

`DESIGN.md` 是当前需求、架构、外部契约和技术决策的事实来源；`TODO.md` 只记录设计与
已验证实现之间的差异，不是独立需求来源。

- 开始实现前，先检查 `DESIGN.md` 和 `TODO.md` 是否已经表达本次需求。
- 如果任务引入或改变架构、协议、CLI/API 契约、安全边界、配额策略、依赖选择、兼容性
  或发布方式，必须按以下顺序更新：

  1. `DESIGN.md`
  2. `TODO.md`
  3. 代码、配置和测试

- 不得先实现技术方案变动，再补写设计文档。
- `TODO.md` 中的事项只有在实现和必要验证均完成后才能勾选。
- 代码、TODO 与设计不一致时，先确定并记录正确设计，再继续实现。
- 如果技术方向或影响范围无法从现有资料确定，应停止相关写操作并向用户确认。

## 3. Rust 实施与错误处理

- 生产 Rust 代码不得使用可能 panic 的 `unwrap()`。
- 文件、网络、IPC、解析、配置、用户输入、系统调用、并发和第三方服务的失败必须返回或
  转换为调用方可以处理的错误。
- 只有类型系统、构造器或同一函数内的穷尽分支已经证明某项内部不变量必然成立时，生产
  代码才可使用 `expect()`。
- 每个 `expect()` 的消息必须具体说明保证其成立的不变量；不得使用 `"should work"`、
  `"cannot fail"` 等泛化描述。
- 不得用 `expect()` 掩盖外部输入、环境或 I/O 的可恢复失败。
- 测试中为了表达断言或构造前置条件，可以合理使用 `unwrap()`、`expect()` 和断言，但
  不得借测试辅助代码逃避生产错误处理。
- 依赖方向和网络边界以 `DESIGN.md` 为准；特别是本地 query 路径不得获得 Figma HTTP
  client，也不得在 cache miss 时隐式联网。

## 4. Cargo manifest 与依赖管理

- 除非用户明确要求直接编辑某个 manifest，否则不得手动编辑 `Cargo.toml`。
- 新增或移除依赖、crate、workspace member 或 crate 元数据时，优先使用能够表达该操作的
  Cargo CLI，例如 `cargo add`、`cargo remove`、`cargo new` 和 `cargo init`。
- 不得用文本补丁、脚本或重定向绕过一个本可由 Cargo CLI 完成的变更。
- Cargo CLI 执行后必须检查实际 manifest、lockfile 和 workspace graph 变化。
- 如果 Cargo CLI 无法表达变更、执行失败或结果不明确，应停止并向用户说明，不能静默退回
  手工修改。
- 新增 crates.io 依赖需要网络审批时，应直接申请范围限定到目标 package 的执行权限；
  不得申请宽泛的依赖修改权限，也不得因网络失败而手工绕过。
- 所有 member manifest 显式保存自身 `version`；不得恢复
  `version.workspace = true` 或 `workspace.package.version`。
- package version 初始化完成后，版本号只能由 GitHub Actions 中的 Semifold CI 根据
  changeset 修改；不得由 Agent 手工编辑或使用 Cargo CLI 本地调整。

## 5. Semifold changeset 与发布

- 仓库使用 Semifold，命令为 `smif`，配置位于 `.changes/config.toml`。
- `main` 是 base branch；Semifold 管理独立的 `release` branch。禁止把 release branch
  配置为 `main`。
- Semifold 配置和 package 列表必须通过 `smif init`、`smif config sync` 等 CLI 维护；
  执行后必须审查生成内容。
- 影响任一受 Semifold 管理 package 的功能、修复、重构、依赖或测试能力的任务，应使用
  `smif commit` 创建 changeset。
- 纯文档、CI、仓库管理和不影响 package 行为的维护工作可以不创建 changeset。
- 不得手工编写 changeset 来绕过 Semifold CLI。
- 默认一个独立任务对应一个独立 changeset；后续独立任务不得修改、合并或复用已有任务的
  changeset，除非用户明确要求将其视为同一任务。
- 创建 changeset 后必须运行 `smif status`，确认解析成功且发布计划符合预期。
- 本地和 Agent 环境严禁执行 `smif version` 与 `smif publish`，包括带 `--dry-run` 的
  调用。版本更新、release branch 写入和发布由 GitHub Actions 中的 `semifold ci` 独占。
- 如果 Semifold 失败、发现的 package 与 workspace 不一致或发布计划不明确，必须停止并
  向用户确认，不得手工模拟其输出。

## 6. 测试与质量门禁

- 验证范围必须与风险相称；不能只说明“代码看起来正确”。
- Rust 变更交付前至少运行与变更相关的以下门禁：

  ```bash
  cargo fmt --all --check
  cargo check --workspace --all-targets --all-features --locked
  cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  INSTA_UPDATE=no cargo test --workspace --all-targets --all-features --locked
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
  ```

- `insta` snapshot 默认必须在 `INSTA_UPDATE=no` 下验证；只有明确审查预期输出变化时才能使用
  `cargo insta review` 接受新 snapshot。
- `.snap.new` 不得提交；已接受的 `.snap` 必须与拥有该行为的测试一起提交。
- 测试和 CI 不得访问真实 Figma API；远端行为使用 mock transport、脱敏或合成 fixture。
- 任何可能消耗 Figma 配额的手工验证都必须获得用户明确授权，并在执行前说明端点和预计
  请求次数。
- Semifold 配置变更至少运行 `smif config sync --check` 和 `smif status`。

## 7. 文档同步

- 完成代码或配置变更后、交付或提交前，必须判断它是否影响用户可见行为、配置契约、
  CLI/API、工作流、示例、架构说明或开发流程。
- 有影响时，必须在同一任务中更新对应现有文档；不得依赖后续任务补齐。
- 文档必须描述当前已验证实现，不得把计划能力写成已经实现。
- 如果无需更新文档，最终交付中必须明确说明原因，不能默认省略文档检查。

## 8. CLI Skill 同步

- 仓库级 CLI 使用 Skill 位于 `.agents/skills/figstash-cli/SKILL.md`，用于向 Agent 提供精炼、
  可执行且与当前实现一致的 Figstash CLI 指引。
- 任何影响 CLI 命令、参数、默认值、输入输出契约、错误码、网络行为、配额语义、本地状态
  副作用或推荐工作流的修改，都必须在同一任务中更新该 Skill。
- 更新 Skill 时必须以当前实现、`figstash schema` 和 `schemas/cli/v1/` 为依据；不得记录尚未
  实现的能力，也不得用 Skill 内容覆盖运行时契约。
- CLI 相关任务交付前必须明确检查 Skill 是否需要更新。需要时必须完成同步；不需要时应在
  最终交付中明确说明原因。

## 9. 任务完成与交付

最终回复必须包含：

- 实际完成的业务和技术产出；
- 执行过的验证及其结果；
- 尚未完成、被阻塞或需要用户决定的事项；
- 本次技术方案相对任务开始时既有设计的变化。

如果技术方案发生变化，应说明变化内容、原因、影响以及如何同步到 `DESIGN.md` 和
`TODO.md`；如果没有变化，也必须明确写明“本次技术方案相对既有设计无变动”。不得把
设计变化隐式藏在代码、配置或依赖中。
