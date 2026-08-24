# Figstash TODO

> 排序规则：P0 必须按顺序完成；P1 在核心 CLI 可用后收敛完整性；P2 才进入 SVG/图片/视觉回归；P3 最后接入 MCP。  
> 技术契约以 [DESIGN.md](./DESIGN.md) 为准。任务完成必须同时满足实现、测试和文档，不以“代码已写”单独判定完成。

## P0 — 核心 Agent CLI（最高优先级）

### P0.1 建立工程和质量门禁

- [x] 创建 Rust workspace 与以下初始 crates：
  - [x] `figstash-core`
  - [x] `figstash-figma`
  - [x] `figstash-store`
  - [x] `figstash-query`
  - [x] `figstash-cli`
- [x] 固定 Rust edition、MSRV 和 workspace dependency policy。
- [x] 配置 Semifold，并在各 member manifest 中显式维护 package version。
- [x] 配置 `rustfmt`、Clippy、单元测试和文档检查。
- [x] 配置 CI：format、clippy、test、JSON Schema contract test。
- [x] 加入 secret scanning 和依赖许可证/漏洞检查。
- [x] 建立 `fixtures/figma`、`schemas/cli/v1` 和 `insta` snapshot 约定。

验收：空功能 workspace 在 macOS/Linux CI 全绿；任何 crate 不得反向依赖 CLI。

### P0.2 固化 Agent CLI 响应契约

- [x] 在 `figstash-core` 定义成功/失败 JSON envelope。
- [x] 定义 `schemaVersion = 1` 和 command-specific `data` schema。
- [x] 定义稳定的 `error.code` taxonomy。
- [x] 实现 exit code 映射。
- [x] 拦截 CLI 参数解析错误并转换为 JSON，禁止 Clap 默认文本污染 stdout。
- [x] 确保 stdout 恰好一个 JSON object；日志只进入 stderr。
- [x] 加入 stdout/stderr/exit code 端到端测试。

验收：成功、参数错误和内部错误均能被 `serde_json` 直接解析，并通过对应 schema。

### P0.3 配置、认证与 secret hygiene

- [x] 实现 OS 标准 data/config 目录解析。
- [x] 支持 `FIGSTASH_DATA_DIR` 和 `FIGSTASH_CONFIG_DIR`。
- [x] 实现配置优先级：CLI > env > config > default。
- [x] 实现 `FIGMA_TOKEN` token source，并明确将其解释为 PAT。
- [x] 定义显式 credential kind；禁止从 token 内容猜测认证类型。
- [x] 实现系统 keyring adapter。
- [x] 实现：
  - [x] `figstash auth set --stdin`
  - [x] `figstash auth status`
  - [x] `figstash auth clear`
- [x] `auth status` 仅报告 credential kind/source/presence，不回显 token，也不伪装成远端 scope 验证。
- [x] 使用 secret wrapper，验证 Debug/log/错误不会出现 token。
- [x] 创建 data dir 时设置当前用户权限。

验收：token 不出现在 argv、stdout、stderr、配置文件、SQLite 和测试 snapshot 中。

### P0.4 Figma URL 与标识符解析

- [x] 支持 design/file/FigJam/branch URL 和直接 file key。
- [x] 解析 query 中的 `node-id`。
- [x] 规范化 `1234-5678` 与 `1234:5678`。
- [x] 校验 URL host、file key、branch key 和 node ID。
- [x] 为 URL decode、无效输入和历史 URL 形式建立 fixture。

验收：所有后续命令共享同一个 parser；不得在 command handler 中重复字符串切割。

### P0.5 配额感知的 Figma gateway

- [x] 定义封闭的 endpoint class 与 Tier 枚举。
- [x] 未知 endpoint fail closed。
- [x] 使用 `reqwest` + rustls 构建唯一 HTTP gateway。
- [x] 实现 credential-aware headers：PAT/plan token 使用 `X-Figma-Token`；OAuth access token 使用 `Authorization: Bearer`。
- [x] 实现 timeout、代理继承和安全 redirect policy。
- [x] 实现 Tier 1 零自动重试策略。
- [x] 实现 Tier 2/3 最多一次安全重试策略。
- [x] 解析 403/404/429/5xx 和 Figma rate-limit headers。
- [x] 每次 attempt 写入本地请求账本。
- [x] 建立 mock transport；测试中禁止直接连接真实 Figma。

验收：代码库中只有 gateway crate 能构造 Figma HTTP 请求；Tier 分类具有穷尽测试。

### P0.6 Durable snapshot store

- [x] 创建 `store-v1` 目录布局。
- [x] 初始化 SQLite schema 与 migration runner。
- [x] 实现 content-addressed zstd blob store。
- [x] 实现原始响应的流式落盘、BLAKE3 和完整 JSON 校验。
- [x] 实现 snapshot manifest、request profile 和 HEAD。
- [x] 实现 per-file/profile 跨进程锁。
- [x] 实现 staging -> transaction -> HEAD 原子提交。
- [x] 实现启动时 orphan staging 检测和安全回收。
- [x] 实现 catalog 备份钩子。

验收：在下载、解析、索引和提交的每个故障点中断进程，旧 HEAD 均保持可读。

### P0.7 完整文件拉取

- [x] 实现 `GET /v1/files/:key` client。
- [x] 支持完整文件、指定 Figma version 和 `geometry=paths` profile。
- [x] 实现 `figstash snapshot pull` 首次拉取。
- [x] 实现 `snapshot pull --force`。
- [x] 禁止使用 `GET file nodes` 作为 node miss 回退。
- [x] 保存 Figma version、lastModified、components、componentSets、styles 和原始 payload。
- [x] 输出实际 Tier 调用数量和 source。
- [x] 添加 200/403/404/429/5xx/timeout/truncated JSON 集成测试。

验收：一次成功 pull 只产生一次 Tier 1；失败 pull 不移动 HEAD；结果 envelope 正确报告请求。

### P0.8 节点索引和 schema-tolerant parser

- [x] 定义关键强类型字段和未知字段保留策略。
- [x] 遍历 DOCUMENT/CANVAS/children 并保持稳定顺序。
- [x] 写入 node、parent、depth、path、bounds、component relation。
- [x] 保存每个节点原始 JSON blob。
- [x] 计算确定性的 subtree hash。
- [x] 建立 name/text FTS5 索引。
- [x] 对未知 node type 生成 warning，但继续提交快照。
- [x] 加入 1k/10k/100k 节点合成 fixture。

验收：任意 node ID 可直接定位；重新索引同一 raw blob 产生相同 node/subtree hashes。

### P0.9 本地节点读取与搜索

- [x] 实现 `figstash node get`：
  - [x] URL 中 node ID
  - [x] `--node`
  - [x] 文件根节点
  - [x] `--depth`
  - [x] `--view compact|raw`
  - [x] 指定 snapshot
- [x] 实现 `figstash node search`：
  - [x] name
  - [x] text
  - [x] type
  - [x] ancestor scope
  - [x] limit/cursor
- [x] 缓存 miss 返回 `snapshot_missing`。
- [x] node miss 返回 `node_not_found` 和本地候选，不访问网络。
- [x] 对所有 query 注入 network-deny transport 并断言零请求。

验收：准备快照后，断网环境下所有 node get/search fixture 通过。

### P0.10 Compact context、tokens 与 components

- [x] 实现 compact node transformer。
- [x] 覆盖 layout、auto-layout、bounds、constraints、Paint、stroke、effect、text 和 instance。
- [x] 从重复值派生稳定 globalVars。
- [x] 将 named styles 与 derived globalVars 分开标记。
- [x] 实现 `figstash tokens get`。
- [x] 实现 `figstash components list`，包含 component sets 和 instance usage。
- [x] 保留 raw view 作为无损出口。
- [x] 为所有核心节点类型建立 `insta` golden snapshots。

验收：输出覆盖被审计项目的 metadata/nodes/globalVars/components/componentSets 能力；`insta` snapshot test 稳定。

### P0.11 快照管理和诊断

- [x] 实现 `figstash snapshot status`。
- [x] 实现 `figstash snapshot list`。
- [x] 实现 `figstash snapshot prune` 删除计划。
- [x] 实现 `snapshot prune --execute` 和无引用 blob GC。
- [x] 实现 `figstash quota status`，明确 `figstash_observed_only`。
- [x] 实现 `figstash doctor` 的本地检查。
- [x] 实现 `figstash doctor --network`，在输出中标明 endpoint/Tier。
- [x] 确保不会自动删除某文件的唯一可用快照。

验收：列表、状态、计划和执行结果全部可由 Agent 解析；删除结果包含删除对象和可恢复性。

### P0.12 核心端到端验收

- [x] 建立完整场景：auth -> pull -> node get/search -> tokens -> components -> status/prune plan。
- [x] 记录每一步预期 API 请求数。
- [x] 验证 pull 后在完全断网环境重复执行 1,000 次 local query，网络请求为零。
- [x] 验证 stdout 没有非 JSON 字节。
- [x] 验证 token 和 signed URL query 不进入日志。
- [x] 编写最小 Agent 使用说明和 JSON schema 索引。
- [x] 验证单次 compact `node get` 同时返回目标子树及其引用的 styles、派生变量和 components，形成零网络的单向 D2C handoff。

### P0.13 Agent 高层语义接口

- [x] 实现 `figstash context <target>`，固定 compact view 并返回节点与引用闭包。
- [x] `context` 缺少 URL node ID 和 `--node` 时返回稳定的 `node_required`，不读取完整文件。
- [x] 实现 `figstash outline <target>` 稀疏树，默认 `--depth 2`。
- [x] 实现 `figstash schema [command]` 的机器可读命令目录和详细契约。
- [x] 保留 `node get/search`、`tokens get` 和 `components list` 作为底层 primitives。
- [x] 为三条高层命令建立 JSON Schema、CLI contract、零网络和参数冲突测试。
- [x] 验证新增接口只复用既有 snapshot，不触发 pull 或改变 HEAD。

验收：Agent 可通过 `outline -> context` 完成节点发现与单向 D2C handoff；不了解节点时不会意外输出整个文件，所有高层 query 的 `meta.network.attempts` 为零。

### P0.14 远端认证身份

- [x] 增加 `GetCurrentUser -> Tier3` 封闭端点分类与 `GET /v1/me` gateway。
- [x] 实现显式在线的 `figstash auth whoami`，返回规范化用户、credential 来源和远端验证状态。
- [x] `auth status` 保持纯本地；`--offline auth whoami` 在读取 credential 和请求前失败。
- [x] 记录实际 Tier 3 尝试并沿用 Tier 2/3 最多一次安全重试策略。
- [x] 补齐 CLI schema、mock transport、错误映射、文档和仓库 CLI Skill。
- [x] 记录当前 PAT 最小 scope 为 `file_content:read` 与 `current_user:read`。

验收：成功 whoami 返回当前 Figma 用户且 `meta.source=figma`；mock 测试验证成功、offline、认证失败和 scope 缺失路径，测试不访问真实 Figma API。

### P0.15 Agent Tier 1 安全工作流

- [x] CLI Skill 要求首次使用先以 `--offline snapshot list/status` 检查目标 file/profile。
- [x] 已有快照时默认只执行带 `--offline` 的本地查询，不主动刷新。
- [x] cache miss、未缓存 profile/version 和刷新场景在执行前逐次告知端点、最大 Tier 1 次数及原因，并等待明确用户授权。
- [x] 明确普通设计读取请求不是 pull 授权，单次授权不延伸到重试、其他文件/profile 或后续刷新。
- [x] `--force` 只用于用户明确要求并授权的刷新；Tier 1 失败不自动重试。

验收：首次加载 Skill 的 Agent 在已有快照时不调用 Tier 1；没有快照时只报告缺失和拟执行成本，未获得明确授权前不调用 `snapshot pull`。

### P0.16 Figma endpoint URL 规范化

- [x] 修复 trailing-slash base URL 追加 path segment 时产生 `/v1//files/:key` 的问题。
- [x] 对普通、version 和 `geometry=paths` 文件请求断言完整规范 URL。
- [x] 使用 loopback HTTP server 验证 production transport 的 GET method 与 PAT `X-Figma-Token` header。
- [x] 检查全部现有 endpoint builder，不允许重复斜杠；测试不得访问真实 Figma API。
- [x] 同步仓库级与全局 CLI Skill 的 endpoint 诊断约束。

验收：`snapshot pull FILE_KEY` 只会构造 `https://api.figma.com/v1/files/FILE_KEY`；回归测试能在出现 `/v1//files/...` 时失败，且不消耗 Figma 配额。

P0 完成定义：Agent 只使用 shell 和 JSON 就能稳定理解整个已缓存 Figma 文件；除显式 pull 外不存在 Tier 1 路径。

## P1 — 核心完整性、稳定性与性能

### P1.1 Metadata 变更探测

- [ ] 实现 `GET /v1/files/:key/meta` Tier 3 client。
- [ ] 已有快照的普通 pull 先比较 version。
- [ ] metadata unchanged 时 Tier 1 为零。
- [ ] metadata scope 缺失时 fail closed，提示 `--force`。
- [ ] 增加 unchanged/changed/missing-scope 请求次数测试。

### P1.2 Snapshot diff

- [ ] 实现 `figstash snapshot diff`。
- [ ] 输出 added/removed/changed/moved nodes。
- [ ] 区分 node 自身字段变化和 descendant-only 变化。
- [ ] 支持按 node/type/path 限定。
- [ ] 为重命名、移动、style/component 变化建立 fixture。

### P1.3 Cache policy 与迁移

- [ ] 实现可选 `stale_after`，只影响状态。
- [ ] 实现 SQLite migration 备份、校验和、rollback-on-failure。
- [ ] 实现从 raw blob 全量重建派生索引。
- [ ] 实现 store integrity check/repair plan。
- [ ] 设计并测试旧 binary 遇到新 store schema 的只读失败行为。

### P1.4 大文件与并发

- [ ] 建立真实脱敏大型 fixture。
- [ ] 测量 pull、index、node get、search 的时间/峰值内存/磁盘。
- [ ] 优化流式解析和 node blob 写入热点。
- [ ] 验证两个进程并发 pull 最多一次 Tier 1。
- [ ] 验证磁盘满、kill -9、SQLite busy 和 blob 损坏场景。
- [ ] 基于数据写入可执行 performance budget。

### P1.5 Feature parity 收敛

- [ ] 将被审计项目所有公开能力逐项转成 acceptance fixture。
- [ ] 核对 URL/node/depth/cacheDir/force refresh 行为覆盖。
- [ ] 增加脱敏的多页面 REST fixture，覆盖普通 `CANVAS`、空页面和页面分隔器的真实 payload；验证完整浏览器 URL 忽略 `p`/`t`、所有页面保持顺序且不会被误建为嵌套层级。
- [ ] 核对所有现有 Figma node、Paint、effect 和 component 类型。
- [ ] 输出明确的 parity report；不将 transport/name compatibility 计为缺口。

### P1.6 平台和发布准备

- [ ] macOS arm64/x86_64 测试与发布产物。
- [ ] Linux x86_64/arm64 测试。
- [ ] Windows data dir、ACL、keyring 和 SQLite 行为验证。
- [x] 确认全部 workspace package 使用 `AGPL-3.0-only`。
- [x] 补齐 crates.io 发布元数据和内部 path dependency 的 registry 版本约束。
- [x] 使用 Cargo metadata、`cargo package` 与逐 package `cargo publish --dry-run` 验证发布 manifest；首次发布前只有无内部依赖的 `figstash-core` 可完成完整 dry-run，其余 package 在 Cargo 确认字段有效后按预期停在 crates.io 尚无 `figstash-core`。
- [ ] 保留 GitHub/crates.io 项目名。
- [ ] 建立 release checklist、SBOM、checksum 和签名策略。

P1 完成定义：核心 CLI 对真实大型文件、崩溃、并发、schema drift 和升级具备可验证的稳定性。

## P2 — SVG、图片和视觉回归（核心 CLI 后）

### P2.1 Geometry SVG compositor

- [ ] 解析 geometry snapshot 的 fill/stroke paths、winding rule 和 transforms。
- [ ] 实现 solid/gradient fills、stroke、opacity 和基础 effects。
- [ ] 实现 clip、mask、boolean/compound paths。
- [ ] 输出独立 `image/svg+xml` 和 fidelity/warning metadata。
- [ ] 对 unsupported 特性返回明确状态，不静默近似。

### P2.2 Figma-rendered SVG snapshot

- [ ] 实现 `svg sync --plan`，计算 Tier 1 批次数。
- [ ] 实现 `svg sync --execute`，固定 Figma version。
- [ ] 使用 `svg_include_node_id=true` 和 outlined text。
- [ ] 下载并持久化实际 SVG 字节。
- [ ] 建立 `node_id -> root SVG/element` 索引。

### P2.3 本地 SVG 切片

- [ ] 实现 ancestor transform/style/opacity 继承。
- [ ] 实现 clip/mask/filter/gradient/pattern/symbol dependency closure。
- [ ] 重写内部 ID，避免切片组合冲突。
- [ ] 内联图片；移除 script、event handler、external entity/URL。
- [ ] 实现 `isolated` 与 `contextual`。
- [ ] 输出 exact/context-dependent/locally-composed/unsupported。

### P2.4 Image fills 与节点 render

- [ ] 实现 Tier 2 image fills listing/fetch。
- [ ] 下载实际字节并用内容 hash 去重。
- [ ] 实现 `render plan`，按格式/scale/options 分组和拆批。
- [ ] 实现 `render run`，只执行持久化 plan。
- [ ] 固定 snapshot version，避免 JSON/图片版本错位。
- [ ] PNG/SVG/JPG/PDF 部分失败必须逐 node 报告。

### P2.5 本地视觉回归

- [ ] 实现 SVG -> PNG 本地渲染。
- [ ] 实现 `visual capture`，明确数据来源和是否发生远端请求。
- [ ] 建立复杂 SVG/Figma export 基准 fixture。
- [ ] 研究并通过 ADR 确定 diff metric、抗锯齿容差和默认阈值。
- [ ] 实现 `visual compare` 的差异图片和机器可读指标。

P2 完成定义：Agent 能在不隐式消耗配额的前提下获取节点视觉资产并进行本地视觉比较。

## P3 — MCP stdio 适配

### P3.1 MCP 契约

- [ ] 基于稳定 application services 设计 tool/resource 粒度。
- [ ] 所有 local tools 明确零网络。
- [ ] 所有 online tools 在名称、description 和结果中展示 Tier 成本。
- [ ] 定义 SVG/图片 resource URI 和生命周期。
- [ ] 不继承旧项目工具名或输出兼容层。

### P3.2 Rust SDK 与实现

- [ ] 固定官方 `rmcp` 稳定版本。
- [ ] 只启用 server + stdio 所需 features。
- [ ] 实现 `figstash mcp --stdio`。
- [ ] handler 直接调用 application service，不 spawn CLI。
- [ ] stdout 只包含 MCP JSON-RPC；日志进入 stderr。
- [ ] 添加 protocol、tool schema、resource 和错误映射测试。

### P3.3 客户端验收

- [ ] MCP Inspector stdio 测试。
- [ ] Codex/Claude Code/Cursor 中至少各验证一个支持的客户端。
- [ ] 验证大节点响应、resource、取消和并发查询。
- [ ] 验证 MCP 反复 query 不产生 Figma 请求。
- [ ] 编写最小配置示例和故障排查。

P3 完成定义：MCP 与 CLI 结果来自同一快照和应用服务，且不会引入新的网络或缓存语义。

## P4 — 可选增强（不进入 v1 阻塞路径）

- [ ] OAuth 浏览器授权和 refresh token 生命周期。
- [ ] Figma Variables 远端读取与本地快照化。
- [ ] 多文件 design-system 跨文件索引。
- [ ] 更细粒度的 Agent pagination/streaming 协议。
- [ ] 本地 Web UI（若未来明确提出；当前不在范围）。
- [ ] 插件/画布写入能力（当前明确不做，除非产品边界重新确认）。

## 每次 PR 的统一完成条件

- [ ] 对应 DESIGN 契约未被无意改变；若改变，先更新 ADR/DESIGN。
- [ ] 单元、集成、contract 和相关 golden 测试通过。
- [ ] 新增网络路径具有 endpoint/Tier 分类和请求次数测试。
- [ ] 新增本地 query 具有“网络即失败”测试。
- [ ] stdout/stderr 和 secret redaction 测试通过。
- [ ] 不引入未说明的自动刷新、自动重试或自动删除。
- [ ] TODO 状态和下一依赖任务已更新。
