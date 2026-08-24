# Figstash 技术方案

> 状态：Draft 0.1（已完成当前范围内的技术决策）  
> 最后更新：2026-08-20  
> 项目形态：全新 Rust 项目，不是 `figma-mcp-cached` 的重构或兼容实现

## 1. 摘要

Figstash 是一个面向 Agent 的、本地优先、配额感知的 Figma 数据 CLI。它通过一次显式的 Figma REST API 全量拉取建立不可变本地快照，之后对文件、节点、样式、设计 token、组件和组件集的读取均在本地完成，避免 View/Collab 席位的 Tier 1 请求上限被日常查询消耗。

CLI 是第一等产品接口。SVG、图片导出与视觉回归在核心 CLI 完成后实现；MCP stdio 仅作为共享核心能力之上的薄适配层，最后接入。项目不注入 Figma Desktop、不加载插件、不修改 Figma 文件，也不提供 HTTP/SSE 服务。

核心原则：

1. **没有隐式网络请求。** 本地查询遇到缓存缺失、过期或节点不存在时返回结构化错误，不自动访问 Figma。
2. **Tier 1 请求必须在命令名和结果中可见。** 只有明确的刷新或渲染命令可以触发 Tier 1。
3. **原始数据可追溯。** 每个派生结果都关联不可变快照、Figma 文件版本、请求配置和原始响应哈希。
4. **Agent 优先。** stdout 始终输出稳定 JSON；无表格、颜色、进度条、交互确认和 TTY 分支。
5. **传输与业务分离。** CLI、未来 MCP 共享同一应用服务，不通过 shell 相互调用。
6. **失败不破坏旧数据。** 刷新必须先完整下载、校验并建索引，最后原子切换当前快照。

## 2. 背景与问题

当前约束如下：

- Figma Desktop 由组织管理，插件导入和 Design/Dev Mode 插件能力不可用。
- 官方 Figma MCP 对 View/Collab 席位的读取调用存在严格月度限制。
- Figma REST API 的 `GET file`、`GET file nodes` 和 `GET image` 均属于 Tier 1；View/Collab 席位当前为“最多 6 次/月”，实际值可能更低。
- 日常 Agent 工作会重复读取同一文件和节点；让每次查询直接访问远端在该额度下不可行。
- 用户需要单机、单用户使用；不需要远程服务、多人共享或旧 MCP 工具兼容。

Figstash 将稀缺的远端读取转换为显式的快照刷新，把高频查询转换为本地索引读取。

## 3. 权威依据与决策记录

### 3.1 已核验资料

| 资料 | 版本/核验时间 | 已确认事实 |
| --- | --- | --- |
| [Figma REST API Rate Limits](https://developers.figma.com/docs/rest-api/rate-limits/) | 2026-08-20 | Tier 1/2/3 分级；View/Collab 限制；PAT、OAuth 和 plan token 的计数维度；429 响应头 |
| [Figma File Endpoints](https://developers.figma.com/docs/rest-api/file-endpoints/) | 2026-08-20 | 文件、节点、图片渲染、image fills、metadata 端点、scope、参数和 URL 有效期 |
| [Figma REST API Authentication](https://developers.figma.com/docs/rest-api/authentication/) | 2026-08-20 | 本地个人工具适合 PAT；文件内容需要 `file_content:read` |
| [Figma Personal Access Tokens](https://developers.figma.com/docs/rest-api/personal-access-tokens/) | 2026-08-20 | PAT 和 plan access token 使用 `X-Figma-Token` 请求头 |
| [Figma OAuth Apps](https://developers.figma.com/docs/rest-api/oauth-apps/) | 2026-08-20 | OAuth access token 使用 `Authorization: Bearer` 请求头 |
| [Figma OpenAPI Specification](https://github.com/figma/rest-api-spec) | 2026-08-20 | 官方 OpenAPI 3.1 和类型；官方明确标注 spec 仍为 beta |
| [`figma-mcp-cached`](https://github.com/Pactortester/Figma-Context-MCP-Cached/tree/d2ba563608aab1e3e06d940e1da789110ac0b440) | commit `d2ba563608aab1e3e06d940e1da789110ac0b440`，npm 1.2.0 | 功能对照基线；此前已完成源码审计，不复用其实现 |
| [官方 MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) | 2026-08-20 | `rmcp` 支持 Rust 服务端与 stdio；仅在 MCP 阶段接入 |

### 3.2 用户已确认的产品决策

- 项目名为 **Figstash**，目录为 `./figstash`。
- 使用 Rust 新建项目，不追求旧仓库的工具名、输出格式或代码兼容。
- 最终能力至少覆盖 `figma-mcp-cached` 的全部用户功能。
- CLI 是主要任务和第一等接口，面向 Agent，不建设人类终端 UI。
- CLI 保留按领域分组的层级命令。
- SVG 在核心 CLI 整体跑通后实现。
- 图片优先级较低，主要用于 Agent 自行进行视觉回归。
- MCP 后续再做，只提供 stdio，不提供 HTTP/SSE。
- 本地单用户威胁模型足够；不要求应用层缓存加密。

### 3.3 本方案新增的技术决策

- stdout 只输出一个 JSON response envelope；stderr 仅输出可关闭的诊断日志。
- 查询服务在依赖图上不能依赖 Figma HTTP client，以结构性保证“本地命令不联网”。
- 使用压缩原始响应 + SQLite 派生索引的两层存储。
- 快照不可变；刷新成功后原子切换 HEAD，失败时继续使用旧快照。
- Figma schema 采用“关键字段强类型 + 未知字段保留”的宽容解析，不将 beta OpenAPI 生成物作为唯一运行时模型。
- 本地配额账本只表示 Figstash 观察到的请求，绝不宣称是 Figma 官方剩余额度。

## 4. 目标、非目标与完成标准

### 4.1 目标

- 使用一次完整 `GET /v1/files/:key` 建立可重复查询的本地快照。
- 支持从 Figma URL 或 file key/node ID 定位文件和节点。
- 支持文件/节点上下文、节点搜索、样式、派生 token、组件和组件集查询。
- 支持强制刷新、快照状态、列表、差异和显式清理。
- 记录每次远端请求的端点类别、Tier、结果和调用来源。
- 对 Agent 提供稳定、版本化、可自动解析的 JSON 契约。
- 最终提供 image fills、节点图片渲染、SVG 本地切片和视觉回归能力。
- 最终通过 stdio MCP 暴露同一组应用能力。

### 4.2 非目标

- 绕过组织策略、注入 Figma Desktop 或侧载插件。
- 读取实时选择、监听画布事件或与 Figma Desktop 建立本地 bridge。
- 写入、创建、修改或删除 Figma 节点。
- 与 `figma-mcp-cached`、官方 Figma MCP 或其他 MCP 的工具名/输出兼容。
- 提供 HTTP、SSE、WebSocket 或局域网访问。
- 在核心 CLI 阶段实现图片、SVG 或 MCP。
- 声称本地账本能够获知其他应用消耗的 Figma 配额。

### 4.3 核心 CLI 完成标准

在没有任何 Figma 网络访问的情况下，Agent 能够针对一个已准备的快照：

- 获取完整文件或任意节点子树；
- 按名称、文本、节点类型和祖先范围搜索；
- 获取 styles、派生 global variables、components 和 component sets；
- 列出、检查、比较和清理本地快照；
- 获得确定性的 JSON 结果和结构化错误；
- 从每个结果中确认数据来自哪个快照以及本次命令产生了多少网络请求。

## 5. Figma 外部契约与配额模型

### 5.1 端点分级

| 能力 | Figma 端点 | Tier | Scope | Figstash 使用阶段 |
| --- | --- | --- | --- | --- |
| 完整文件快照 | `GET /v1/files/:key` | 1 | `file_content:read` | 核心 CLI |
| 指定远端节点 | `GET /v1/files/:key/nodes` | 1 | `file_content:read` | 正常流程禁用；不用于缓存 miss 回退 |
| 节点图片/SVG 渲染 | `GET /v1/images/:key` | 1 | `file_content:read` | 视觉阶段 |
| 获取 image fill URL | `GET /v1/files/:key/images` | 2 | `file_content:read` | 视觉阶段 |
| 文件 metadata/version 探测 | `GET /v1/files/:key/meta` | 3 | `file_metadata:read` | 核心 CLI 优化 |
| 当前认证用户 | `GET /v1/me` | 3 | `current_user:read` | 核心 CLI |

所有 Figma endpoint URL 必须序列化为规范的单斜杠路径；通过 base URL 追加 path segment 时必须先移除末尾空 segment，禁止产生 `/v1//...`。gateway 测试必须断言完整序列化 URL，并使用仅监听 loopback 的本地 HTTP server 验证 production transport 的 method 和 credential header；测试不得访问真实 Figma API。

当前官方限制中，View/Collab 席位的 Tier 1 为最多 6 次/月、Tier 2 为最多 5 次/分钟、Tier 3 为最多 10 次/分钟；Figma 明确保留调整限制及在高负载下降低实际额度的权利。因此这些数字是运行时策略的初始值，不是硬编码的产品承诺。

### 5.2 计数语义

- PAT 按 Figma 用户和资源所属 plan 计数。
- OAuth 按用户、plan 和 app 计数。
- 一个 Figstash CLI 命令可能产生零个、一个或多个 REST 请求；结果必须报告实际尝试数。
- 下载 Figma 返回的 signed asset URL 不是第二次 `api.figma.com/v1/...` REST 调用，但 URL 有有效期，必须尽快持久化实际字节。
- 本地账本无法看到官方 MCP、其他 OAuth app 或其他本地工具的用量。

### 5.3 快照刷新策略

`figstash snapshot pull` 是核心阶段唯一能调用 Tier 1 文件内容端点的命令。

Agent 集成必须把 Tier 1 视为需要逐次授权的稀缺操作：首次处理任何目标时，先用 `--offline snapshot list/status` 检查对应 file/profile 的本地快照；已有可用快照时直接进入全离线查询，不得为了“确保最新”主动 pull。普通的“查看设计”“读取节点”或“按 Figma 实现”请求不等同于刷新授权。缓存缺失、指定了未缓存 profile/version 或用户要求刷新时，Agent 必须在执行前告知用户将调用的 Tier 1 端点、预计最大请求数、需要联网的原因及是否使用 `--force`，并获得针对该次操作的明确授权。授权不延伸到失败重试、其他文件、其他 request profile 或后续刷新。

默认流程：

1. 解析 file key、branch key、node ID 和请求 profile。
2. 获取该 file/profile 的本地 HEAD。
3. 若不存在本地快照，明确调用一次 Tier 1 `GET file`。
4. 若已有快照且未指定 `--force`，先调用 Tier 3 metadata：
   - version 未变化且本地 profile 已满足：返回 `unchanged`，Tier 1 为零；
   - version 变化：调用一次 Tier 1 `GET file`；
   - metadata scope 不可用：不自动退化为 Tier 1，返回 `metadata_unavailable`，提示调用方显式使用 `--force`；
5. `--force` 跳过变更判断，明确调用一次 Tier 1。
6. `--version <figma-version>` 直接拉取指定版本，不做 current metadata 判断。

典型成本：

| 场景 | Tier 3 | Tier 1 |
| --- | ---: | ---: |
| 首次 pull | 0 | 1 |
| 已有快照且远端未变化 | 1 | 0 |
| 已有快照且远端已变化 | 1 | 1 |
| `--force` | 0 | 1 |

`geometry=paths` 不增加请求次数，但显著增加响应体积。核心 CLI 默认不请求几何；调用方通过 `--geometry paths` 建立包含向量路径的独立请求 profile。已有普通快照不能被静默“升级”为 geometry 快照，因为升级需要新的 Tier 1 调用。

### 5.4 重试策略

- Tier 1：不自动重试。请求一旦离开进程，Figstash 无法可靠判断 Figma 是否已经计入调用，重试必须由 Agent 再次显式发起。
- Tier 2/3：仅对连接建立失败和 5xx 做最多一次带抖动重试；429 直接返回 `Retry-After` 等信息，不在 CLI 内休眠等待。
- 所有端点：403、404 和 schema 错误不重试。
- 旧快照在任何刷新失败后仍保持可读，但刷新命令必须返回失败，不能伪装为成功。

## 6. 功能对照基线

最终 v1 以能力而非旧接口形式覆盖被审计项目：

| `figma-mcp-cached` 能力 | Figstash 能力 | 优先级 |
| --- | --- | --- |
| 准备并缓存完整 Figma 文件 | `snapshot pull` + 不可变快照 | P0 |
| node ID 存在性检查 | 本地 node index | P0 |
| 强制刷新 | `snapshot pull --force` | P0 |
| 获取完整文件或指定节点上下文 | `node get` | P0 |
| metadata/nodes/globalVars/components/componentSets | `node get`、`tokens get`、`components list` | P0 |
| 可配置缓存目录 | 配置文件与 `FIGSTASH_DATA_DIR` | P0 |
| TTL 状态 | 可选 `stale_after`，只影响状态，不触发刷新 | P1 |
| 缓存列表与清理 | `snapshot list/status/prune` | P0/P1 |
| SVG/PNG 和 image fill 下载 | `svg`、`assets`、`render` | P2 |
| stdio MCP | `figstash mcp --stdio` | P3 |

明确不继承的旧行为：HTTP transport、自动 TTL 刷新、node miss 自动远端获取、429 多次重试、旧 YAML/JSON 格式和旧工具名。

## 7. 总体架构

```text
                        ┌─────────────────────┐
                        │  figstash CLI       │
                        │  JSON in / JSON out │
                        └──────────┬──────────┘
                                   │
                        ┌──────────▼──────────┐
                        │ Application services│
                        │ commands + queries  │
                        └──────┬────────┬─────┘
                               │        │
                 local queries │        │ explicit online commands
                               │        │
                 ┌─────────────▼─┐   ┌──▼────────────────┐
                 │ Snapshot store│   │ Figma REST client │
                 │ SQLite + blobs│   │ quota + auth      │
                 └──────┬────────┘   └─────────┬─────────┘
                        │                      │
                 ┌──────▼────────┐             │
                 │ Transform/query│             │
                 │ compact context│             │
                 └───────────────┘             │
                                               ▼
                                        api.figma.com

Later:
  figstash-svg / figstash-visual ── use the same store and online gateway
  figstash-mcp ──────────────────── calls the same application services
```

关键依赖规则：

- Query service 只能依赖 `SnapshotRepository`，不能依赖 `FigmaClient`。
- Online command service 必须通过 `QuotaAwareFigmaGateway` 访问网络，禁止直接使用 `reqwest::Client`。
- CLI 和未来 MCP 只负责参数/结果适配，不包含业务逻辑。
- Store 不理解 CLI/MCP；其公开契约是快照、索引、blob 和账本 repository trait。
- Transform 层必须能对固定 fixture 离线运行。

## 8. Rust workspace 规划

```text
figstash/
├── Cargo.toml
├── Cargo.lock
├── DESIGN.md
├── TODO.md
├── crates/
│   ├── figstash-core/       # 领域类型、应用服务、错误和 JSON envelope
│   ├── figstash-figma/      # REST、认证、URL 解析、Tier 分类
│   ├── figstash-store/      # SQLite、blob、锁、迁移
│   ├── figstash-query/      # 索引构建、上下文压缩、搜索、diff
│   ├── figstash-cli/        # 唯一初始二进制；内含可发布的 CLI schema 镜像
│   ├── figstash-svg/        # P2
│   ├── figstash-visual/     # P2
│   └── figstash-mcp/        # P3
├── schemas/
│   └── cli/v1/              # 命令输出 JSON Schema
├── fixtures/
│   └── figma/               # 脱敏或合成的 REST 输入
├── crates/*/tests/
│   └── snapshots/           # insta 管理的预期输出
└── xtask/                   # schema 校验、fixture 维护、发布检查
```

初始依赖方向：

```text
figstash-cli -> core + figma + store + query
query        -> core
figma        -> core
store        -> core
core         -> no infrastructure crate
```

P2/P3 crate 在对应阶段开始前不创建空壳，避免提前固化无用接口。

### 8.1 计划使用的基础库

| 领域 | Rust 库 | 原因 |
| --- | --- | --- |
| CLI | `clap` | 分层 subcommand、稳定参数解析；解析错误会被适配为 JSON |
| Async/HTTP | `tokio`、`reqwest` + rustls | 流式下载、代理环境兼容、无 OpenSSL 运行时依赖 |
| 序列化 | `serde`、`serde_json` | Figma payload 与 CLI 契约 |
| 存储 | `rusqlite`（bundled SQLite） | 单进程/多进程本地索引、事务和 FTS5 |
| 压缩/哈希 | `zstd`、`blake3` | 大型 JSON/节点 blob 与内容寻址 |
| 配置路径 | `directories` | OS 标准 data/config 目录 |
| Secret | `secrecy`、`zeroize`、系统 keyring adapter | 防止 token 进入 Debug/日志；PAT 不写普通配置 |
| 日志/错误 | `tracing`、`thiserror` | 结构化诊断与稳定错误映射 |
| 测试 | `insta`、`tempfile`、`assert_cmd`、HTTP mock | 可 review 的 golden snapshot 与全链路离线测试 |

依赖版本由 `Cargo.lock` 固定；方案文档不绑定易过期的具体 patch 版本。

## 9. CLI 契约

### 9.1 命令面

核心阶段：

```bash
# 认证和诊断
figstash auth set --stdin
figstash auth status
figstash auth whoami
figstash auth clear
figstash doctor
figstash doctor --network
figstash quota status

# 快照
figstash snapshot pull <figma-url-or-file-key>
figstash snapshot pull <figma-url-or-file-key> --force
figstash snapshot pull <figma-url-or-file-key> --geometry paths
figstash snapshot status <file-key>
figstash snapshot list
figstash snapshot diff <file-key> <snapshot-a> <snapshot-b>
figstash snapshot prune
figstash snapshot prune --execute

# 本地数据
figstash context <figma-node-url-or-file-key> [--node <node-id>] [--depth <n>]
figstash outline <figma-url-or-file-key> [--node <node-id>] [--depth <n>]
figstash schema [command]
figstash node get <figma-url-or-file-key> [--node <node-id>] [--depth <n>]
figstash node get <figma-url-or-file-key> --view raw
figstash node search <file-key> [--name <text>] [--text <text>] [--type <type>]
figstash tokens get <file-key>
figstash components list <file-key>
```

后续阶段：

```bash
figstash svg sync <file-key> --plan
figstash svg sync <file-key> --execute
figstash svg get <figma-node-url> --mode isolated -o node.svg
figstash svg get <figma-node-url> --mode contextual -o node.svg

figstash assets list <file-key>
figstash assets fetch <file-key> -o ./assets
figstash render plan <figma-node-url> --format png
figstash render run <plan-id> -o ./assets
figstash visual capture <figma-node-url> -o actual.png
figstash visual compare expected.png actual.png

figstash mcp --stdio
```

### 9.2 输入规范

- 支持 `figma.com/design/...`、`figma.com/file/...`、FigJam/branch URL 以及直接 file key。
- URL 中 `node-id=1234-5678` 统一规范化为 REST ID `1234:5678`。
- 浏览器链接中的展示态参数（例如 `p`、`t`）不参与快照身份或请求 profile；解析器只提取受支持的资源类型、file/branch key 和 `node-id`，其余参数忽略。因而从浏览器地址栏复制的完整 Design URL 可直接作为 `<figma-url-or-file-key>` 传入。
- file key、branch key 和 node ID 在进入应用层前完成语法校验和 URL decode。
- token 不允许通过命令行参数传入，防止出现在进程列表和 shell history。
- `--snapshot` 未指定时读取对应 request profile 的本地 HEAD。
- `context` 是 Agent 首选的 compact D2C 入口；目标必须由 URL 或 `--node` 明确到节点，缺失时返回 `node_required` 并引导调用 `outline`。
- `outline` 是稀疏结构发现入口，默认 `--depth 2`，只返回节点标识、名称、类型、bounds、path、child count 和受深度限制的 children。
- URL node ID 与显式 `--node` 同时存在时必须规范化后相等，否则返回 `invalid_arguments`。
- `context`、`outline`、`schema` 和所有底层 query 永不因 cache miss 隐式联网；`snapshot_missing` 只提供显式 pull 建议。
- `schema` 不带参数时列出机器可读的命令目录；传入稳定 command name 时返回参数/结果 schema、错误、网络和本地写入效果及示例。
- 全局 `--offline` 使所有在线 command 在发送请求前返回 `offline_mode`；本地 query 行为不变。

#### 9.2.1 页面与页面分隔器

- `snapshot pull` 对 Design URL 始终调用一次完整文件端点；URL 中的 `node-id` 只作为后续本地查询目标，不把远端请求缩小为单个节点，也不增加请求次数。
- Figma REST 文件树按 `DOCUMENT -> CANVAS(page) -> children` 处理。所有页面都是 `DOCUMENT` 下保持原始顺序的 `CANVAS` 兄弟节点，不建立“二级页面”层级。
- Figma 左侧 Pages 面板中的分隔标题是页面组织 UI，不代表其后的页面成为 REST 树中的子页面。Figstash 必须继续索引分隔标题之后的每个 `CANVAS`，不得因空页面、特殊名称或未知字段跳过后续页面。
- Plugin API 提供页面分隔器标识，但当前 REST node schema 未提供可依赖的等价字段。核心解析器因此保留原始 JSON 和未知字段，并将 REST payload 中的分隔器表示按普通、可能为空的 `CANVAS` 安全处理；不得仅凭名称或视觉缩进推断层级。
- 后续若真实、脱敏的 REST fixture 提供稳定且权威的分隔器信号，可在 `outline` 中增加非破坏性的展示分类；该分类不能改变 node ID、parent、sibling order、path、快照内容或本地查询语义。

### 9.3 stdout/stderr 规范

- stdout：无论成功或失败，恰好一个 UTF-8 JSON object，末尾换行。
- stderr：默认仅输出 warning/error 级诊断；`--log-level off` 完全关闭。
- stdout 禁止进度文字、ANSI escape、表格、Markdown 和多个 JSON 文档。
- 二进制内容默认写入显式 `-o` 路径；只有 `--stdout` 才允许把 SVG 等内容写到 stdout，该命令的 stdout 不再使用 envelope，并在帮助/schema 中单独标记。
- CLI 不发起交互式确认。危险动作采用 `plan`/`--execute` 两步或明确的 `--force`。

### 9.4 JSON response envelope

成功：

```json
{
  "schemaVersion": 1,
  "ok": true,
  "data": {},
  "meta": {
    "command": "node.get",
    "source": "cache",
    "snapshotId": "b3:...",
    "figmaVersion": "1234567890",
    "network": {
      "attempts": 0,
      "tier1": 0,
      "tier2": 0,
      "tier3": 0
    },
    "durationMs": 12
  },
  "warnings": []
}
```

失败：

```json
{
  "schemaVersion": 1,
  "ok": false,
  "error": {
    "code": "snapshot_missing",
    "message": "No local snapshot matches this file and request profile.",
    "details": {
      "fileKey": "...",
      "suggestedCommand": "figstash snapshot pull ..."
    },
    "retryable": false
  },
  "meta": {
    "command": "node.get",
    "source": "none",
    "network": {
      "attempts": 0,
      "tier1": 0,
      "tier2": 0,
      "tier3": 0
    }
  },
  "warnings": []
}
```

字段约束：

- `schemaVersion` 只在发生破坏性 JSON 契约变化时增加。
- `data` 与 `error` 互斥。
- `warnings` 始终存在，且不用于表示命令失败。
- `source` 取值为 `cache`、`figma`、`mixed` 或 `none`。
- `network` 记录实际已尝试请求，而不是预估成本。
- 每个命令的 `data` 有独立 JSON Schema；CI 校验 fixture 与 schema 一致。

### 9.5 Exit code

| Exit code | 类别 | 示例 |
| ---: | --- | --- |
| 0 | 成功 | 命中缓存、刷新成功、无变更 |
| 2 | 输入/CLI 契约错误 | URL、node ID、参数组合非法 |
| 3 | 认证错误 | token 缺失、失效、scope 不足 |
| 4 | 本地资源不存在 | snapshot/node/component 不存在 |
| 5 | 网络或 Figma 服务错误 | timeout、5xx、无效响应 |
| 6 | 配额/策略阻止 | 429、offline、缺少明确 Tier 1 授权 |
| 7 | 缓存/数据完整性错误 | hash 不匹配、迁移失败 |
| 8 | 未分类内部错误 | bug；必须带 trace ID |

Agent 应以 JSON `error.code` 为主要分支依据，exit code 仅用于粗粒度控制。

## 10. 应用服务与数据流

### 10.1 Command/query 分离

Online commands：

- `SnapshotPull`
- `SvgSync`（P2）
- `AssetFetch`（P2）
- `RenderRun`（P2）
- `DoctorNetworkProbe`

Local queries/commands：

- `ContextGet`
- `OutlineGet`
- `SchemaGet`
- `SnapshotStatus/List/Diff/PrunePlan`
- `NodeGet/Search`
- `TokensGet`
- `ComponentsList`
- `QuotaStatus`
- `SvgGet`（P2）
- `VisualCompare`（P2）

本地 query 的构造函数不接收任何网络 gateway，从类型依赖上阻止缓存 miss 回源。

### 10.2 `snapshot pull` 时序

```text
Agent
  -> CLI: snapshot pull URL
  -> URL parser: file key / profile
  -> Store: acquire per-file/profile lock
  -> Store: read HEAD
  -> Figma gateway: optional Tier 3 metadata
  -> Figma gateway: explicit Tier 1 GET file when required
  -> Staging: stream raw response, hash, validate envelope
  -> Indexer: walk document and build node/style/component indexes
  -> Store: commit immutable snapshot and request ledger
  -> Store: atomically update HEAD
  -> CLI: JSON envelope
```

若任何 staging/index 步骤失败，临时数据可在下次启动时回收，旧 HEAD 不变。

### 10.3 `node get` 时序

```text
Agent
  -> CLI: node get URL
  -> URL parser
  -> Store: resolve HEAD/snapshot
  -> Node index: locate node
  -> Query: load node/subtree, apply depth
  -> Transformer: compact or raw view
  -> CLI: JSON envelope with network.attempts = 0
```

`context` 复用相同的 local query service，但固定 compact view、要求显式 node，并把目标子树实际引用的 named styles、derived global vars、components 和 component sets 一并返回。`outline` 复用 node index 构建稀疏树，不加载 Paint、Effect、typography、component overrides 或 vector path payload。两者的应用服务均不持有网络 gateway。

## 11. Figma 数据模型与解析

### 11.1 Schema 策略

Figma 官方 OpenAPI spec 仍为 beta。Figstash 不把完整 codegen 类型作为持久化边界：

- 文件 envelope、Node 公共字段、Paint、Component、ComponentSet 和 Style 使用明确 Rust 类型。
- 节点类型特有字段通过 `serde(flatten)`/`serde_json::Value` 保留，未知字段不能因客户端版本落后而丢失。
- 未知 node type 被索引为 `UNKNOWN`，原始 JSON 保留，本地 raw view 仍可读取。
- 每个保存的快照记录使用的 Figstash parser/schema 版本。
- CI 使用固定 commit 的官方 OpenAPI 进行兼容性检查，但运行时不联网取 schema。

### 11.2 节点规范化

每个节点索引至少包含：

- `snapshot_id`
- `node_id`
- `parent_id`
- `node_type`
- `name`
- `depth`
- `sibling_order`
- `path_ids`
- `visible`
- `absolute_bounding_box`
- `component_id`
- 可搜索文本摘要
- `node_json` 的压缩 blob 引用
- `subtree_hash`

遍历必须是确定性的，保持 Figma children 顺序。`subtree_hash` 基于规范化节点字段和有序 child hashes，用于本地 diff，不作为 Figma 版本的替代。

### 11.3 Compact context

`node get` 默认 `--view compact`，包含：

- 文件和快照 metadata；
- 节点 ID、name、type、可见性和层级；
- 布局、尺寸、相对/绝对位置和约束；
- auto-layout、padding、gap 和 sizing；
- fills、strokes、effects、opacity、radius；
- text characters 与 typography；
- component/instance 关联；
- image reference 与 export settings；
- children，受 `--depth` 控制；
- 引用到的 styles/global vars/components/component sets。

`--view raw` 返回快照中该节点的原始 Figma JSON。默认不设置 `--depth` 时返回完整子树；不得静默截断。调用方负责用 `--depth`、搜索或更小节点控制输出规模。

### 11.4 Tokens 与 components

`tokens get` 在核心阶段读取本地快照并返回：

- Figma 顶层 named styles；
- compact transform 生成的可复用 `globalVars`；
- 颜色、字体、间距、圆角、effect 等归一化值和引用位置。

这里的 `globalVars` 是从文件内容派生的复用变量，不冒充 Figma Variables API。远端 Variables 端点属于独立能力，核心阶段不调用。

`components list` 返回快照中的 `components`、`componentSets`，以及可从 node index 建立的 instance 使用关系。

## 12. 本地持久化设计

### 12.1 数据目录

快照是稀缺配额换来的耐久数据，不放入可能被系统自动清理的临时 cache 目录。

默认位置：

- macOS：`~/Library/Application Support/Figstash`
- Linux：`$XDG_DATA_HOME/figstash`，缺省为 `~/.local/share/figstash`
- Windows：`%LOCALAPPDATA%\Figstash`（P1 验证）

通过 `FIGSTASH_DATA_DIR` 覆盖。配置使用 OS config 目录或 `FIGSTASH_CONFIG_DIR`。代码不得复用或重定义 `$HOME` 等系统变量。

### 12.2 目录布局

```text
Figstash/
├── store-v1/
│   ├── catalog.sqlite3
│   ├── blobs/
│   │   └── b3/<prefix>/<hash>.zst
│   ├── staging/
│   ├── locks/
│   └── backups/
└── logs/                  # 仅在显式启用文件日志时创建
```

### 12.3 SQLite schema（逻辑）

```text
snapshots(
  id, file_key, figma_version, request_profile,
  file_name, fetched_at, last_modified,
  raw_blob_hash, raw_size, node_count,
  parser_version, status
)

heads(file_key, request_profile, snapshot_id)

nodes(
  snapshot_id, node_id, parent_id, node_type, name,
  depth, sibling_order, path_ids,
  visible, x, y, width, height,
  component_id, node_blob_hash, subtree_hash
)

node_fts(snapshot_id, node_id, name, text_content)

styles(snapshot_id, style_id, style_type, name, json_blob_hash)
components(snapshot_id, component_id, node_id, name, json_blob_hash)
component_sets(snapshot_id, component_set_id, node_id, name, json_blob_hash)
derived_artifacts(snapshot_id, kind, schema_version, blob_hash)

api_attempts(
  id, started_at, command, endpoint_class, tier,
  file_key, request_profile, http_status,
  completed_at, retry_of, error_code
)

schema_migrations(version, applied_at, checksum)
```

大对象使用 zstd blob；SQLite 保存索引、关系和 hash。原始完整响应始终保留，允许在不重新访问 Figma 的情况下重建新版本派生索引。

### 12.4 原子性和并发

- 每个 `(file_key, request_profile)` 使用跨进程文件锁。
- 获得锁后再次检查 HEAD/version，避免两个 CLI 进程重复拉取。
- 网络响应流式写入 staging 文件，同时计算 BLAKE3。
- 完整 JSON 校验、索引和派生产物都在 staging 中完成。
- catalog 事务提交 snapshot 后才更新 `heads`。
- blob 采用 content-addressed path；重复内容不重复存储。
- 进程崩溃后，启动时只清理没有被 catalog 引用的过期 staging 数据。
- `snapshot prune` 默认只返回删除计划；`--execute` 才删除。blob GC 只删除引用计数为零的对象。

### 12.5 TTL

- 配置可选 `stale_after`，默认未设置。
- TTL 只影响 `snapshot status` 的 `fresh/stale` 标记。
- TTL 到期不触发 pull、不让 query 失败，也不删除数据。
- Agent 必须显式决定是否刷新。

## 13. 认证与配置

### 13.1 Token source

核心阶段只接受 PAT，优先级为：

1. 当前进程的 `FIGMA_TOKEN`，明确解释为 PAT；
2. 系统 keyring 中的 Figstash PAT；
3. 未配置，返回 `auth_missing`。

`figstash auth set --stdin` 从 stdin 读取 PAT 并写入系统 keyring；token 不回显、不写普通配置、不出现在 args、JSON 和日志。`auth status` 只返回 credential kind、token source 和是否存在，不返回 token 内容，也不声称已验证远端 scope。`auth whoami` 是显式在线命令，调用 Tier 3 `GET /v1/me` 验证 credential 并返回规范化的 `id`、`handle`、`email` 和 `avatarUrl`；成功结果同时返回 credential kind/source 和 `remoteValidated=true`。它不消耗 Tier 1 文件内容额度，但仍受 Tier 3 限流和既有安全重试策略约束。`--offline auth whoami` 在读取 credential 或发送请求前返回 `offline_mode`。`auth clear` 删除 keyring 项。

当前完整 CLI 的 PAT 最小 scope 是 `file_content:read` 与 `current_user:read`：前者用于快照拉取，后者用于 `auth whoami`。P1 metadata 探测落地后还需要 `file_metadata:read`。Figstash 不从 token 字符串猜测或声称已授予 scope；远端 401/403 按稳定认证错误返回。

网络层使用显式 `CredentialKind`，不能从 token 字符串猜测类型：

- `PersonalAccessToken` 与未来的 `PlanAccessToken` 使用 `X-Figma-Token: <token>`；
- 未来的 `OAuthAccessToken` 使用 `Authorization: Bearer <token>`。

P0 只实现 `PersonalAccessToken`；其余类型保留清晰的扩展点。OAuth 浏览器授权、refresh token 生命周期和 plan token 配置均不阻塞核心 CLI。

### 13.2 配置

配置项包括：

- data/config 目录；
- `stale_after`；
- HTTP connect/total timeout；
- 代理继承策略；
- 默认日志级别；
- Tier policy override（只允许更保守，放宽需要显式 CLI flag）；
- 单文件最大下载大小保护；
- P2 的 asset 输出目录与视觉阈值。

优先级：显式 CLI flag > 环境变量 > 配置文件 > 内置安全默认值。

## 14. 配额保护与可观察性

### 14.1 Endpoint classifier

所有 Figma 请求在构造前必须映射到封闭枚举：

```text
GetFile       -> Tier1
GetFileNodes  -> Tier1
GetImages     -> Tier1
GetImageFills -> Tier2
GetFileMeta   -> Tier3
GetCurrentUser -> Tier3
```

未知端点默认禁止，而不是假设低 Tier。

### 14.2 本地账本

`quota status` 返回：

- Figstash 在当前自然月观察到的 attempted/succeeded 请求；
- 按 Tier、file key、endpoint class 和 command 聚合；
- 最近一次 429 的 `Retry-After`、plan tier 和 rate limit type；
- 明确的 `scope: "figstash_observed_only"`；
- 官方限制策略的来源链接和内置策略更新时间。

它不输出伪造的 `remaining=...`。可计算 `localBudgetEstimate`，但必须标注没有包含其他客户端用量且不是 Figma 保证值。

### 14.3 日志

- 每个命令有 trace ID，错误 envelope 包含该 ID。
- HTTP 日志只记录 method、endpoint class、Tier、status、耗时和响应大小。
- URL 中 file key 默认可记录；node ID、文件名和设计文本默认不写日志。
- Authorization、signed URL query、原始响应和 token 永不写日志。
- 不提供 telemetry、崩溃上传或远端 analytics。

## 15. 错误与降级

| 场景 | 行为 |
| --- | --- |
| 无本地快照 | query 返回 `snapshot_missing`，零网络 |
| node 不存在 | 返回 `node_not_found` 和相近 node ID/name 候选，零网络 |
| metadata scope 缺失 | pull 返回 `metadata_unavailable`；不隐式调用 Tier 1 |
| Tier 1 429 | 返回 `rate_limited`、响应头和本地旧快照信息；不重试 |
| Tier 1 5xx/timeout | 当前 refresh 失败；旧 HEAD 保持；不重试 |
| Figma 新增未知字段/type | 原始数据保存；已知字段继续索引；warning 标记解析覆盖率 |
| 原始响应损坏 | 不提交 snapshot；保留旧 HEAD |
| 索引 schema 过期 | 从本地 raw blob 重建，不访问 Figma |
| 磁盘空间不足 | staging 失败并清理；旧快照不受影响 |
| 并发 pull | 文件锁后复查；复用新 HEAD 或等待/返回 busy，不重复请求 |

不允许的降级：用旧数据冒充最新刷新成功、静默丢字段、缓存 miss 回源、自动删除唯一快照。

## 16. 性能设计

目标不是极端吞吐，而是在大型文件上保持 Agent 可接受的确定性延迟。

- HTTP 响应流式落盘并增量计算 hash，避免复制完整字节缓冲。
- 首次索引采用单次深度优先遍历，时间复杂度 O(nodes)。
- node lookup 使用 `(snapshot_id, node_id)` 主键索引。
- name/text search 使用 FTS5，默认返回 100 条和稳定 cursor。
- 每个 node JSON 独立压缩，读取一个子树无需解压完整文件。
- compact 派生产物按 `(snapshot_id, transformer_schema)` 缓存。
- 完整 raw blob 保留，用于迁移和重新索引。

需要基准 fixture：1k、10k、100k 节点和至少一个大文本/图片引用文件。性能目标在真实 fixture 建立后固化，不能凭空写入毫秒承诺。

## 17. SVG、图片与视觉回归（P2）

该阶段不阻塞核心 CLI，但属于最终功能完整性范围。

### 17.1 SVG 两层来源

1. **Geometry compositor**：从 `geometry=paths` 快照的 `fillGeometry`、`strokeGeometry`、Paint、Transform、Mask 和 Effect 本地生成 SVG；不增加远端请求，保真度按能力标记。
2. **Figma-rendered SVG snapshot**：批量请求顶层 renderable nodes，参数包含 `format=svg`、`svg_include_node_id=true`、`svg_outline_text=true` 和快照 `version`；下载后建立 `node_id -> root SVG element` 索引。

本地切片必须处理祖先 transform/opacity/clip/mask/filter，递归复制 `<defs>` 引用、重写 ID、内联外部图片并清理脚本/外链。不得简单复制单个 `<g>`。

`svg get` 支持：

- `isolated`：节点及其 descendants 的独立导出语义；
- `contextual`：保留祖先裁剪、透明度和 transform，但排除无关 siblings。

结果标记 `exact`、`context_dependent`、`locally_composed` 或 `unsupported`，不伪装为像素级一致。

### 17.2 图片

- `assets fetch` 使用 Tier 2 image fills endpoint，下载实际字节并 content-addressed 存储，不只缓存最长 14 天的 URL。
- `render plan` 按 `(file version, format, scale, SVG options)` 分组并计算预计 Tier 1 请求数。
- `render run` 只执行已保存 plan；同组 node IDs 尽量批量，URL 长度要求拆分时在 plan 中展示额外调用。
- Figma rendered URL 最长约 30 天；成功后立即下载并缓存实际字节。
- 同一 `(snapshot, node, render options)` 命中本地缓存时不访问 Figma。

### 17.3 视觉回归

- `visual capture` 复用本地 SVG/raster 或显式 render plan，不隐藏远端成本。
- `visual compare` 完全本地，输出差异图片、尺寸/alpha 归一化信息和机器可读指标。
- 比较阈值、抗锯齿容差和 perceptual metric 在建立真实 fixture 后固化为单独 ADR；核心阶段不提前承诺算法。

## 18. MCP 适配（P3）

- 使用官方 Rust SDK `rmcp` 的稳定发行版，在实现时固定版本。
- 只启用 server + stdio transport；不编译 HTTP server feature。
- 二进制入口为 `figstash mcp --stdio`，stdout 专用于 MCP JSON-RPC，日志只写 stderr。
- MCP tool handler 直接调用 application service，不执行 `figstash` 子进程。
- 本地 query tool 保持零网络；online tool 名称和 schema 明确标注 Tier 成本。
- SVG 等大对象通过 MCP resource/resource link 返回，结构化 metadata 通过 tool result 返回。
- MCP tool/output 契约独立版本化，不承诺兼容旧项目。

## 19. 安全与隐私边界

威胁模型：单用户受控设备，攻击面主要是 secret 泄漏、意外网络请求、缓存权限和不可信 SVG；不建设多租户或远程认证。

要求：

- 无监听端口、无 HTTP/SSE、无 telemetry。
- data dir 尽可能设为 `0700`，数据文件 `0600`；Windows 使用等价用户 ACL。
- PAT 仅存在环境或系统 keyring；使用 secret wrapper，禁止 Debug 输出。
- HTTP 只允许 HTTPS `api.figma.com` 和经 Figma 响应获得的 asset host；redirect 应执行 host/scheme policy。
- SVG parser 禁用 DTD/external entities；输出移除 script、事件属性、`foreignObject` 和非白名单外链。
- SQL 参数化，node/name 不拼接 SQL。
- 解压、JSON/XML 深度、blob 大小和节点数设置资源上限，防止损坏快照耗尽内存。
- 通过 Agent 调用时，CLI 输出可能被 Agent 客户端发送给远端模型；这是客户端/模型边界，不由 stdio 或本地 CLI 消除。

缓存不做应用层加密；依赖设备磁盘加密和 OS 用户边界。该决策符合已确认的使用场景。

## 20. 测试方案

### 20.1 单元测试

- Figma URL/file key/node ID 解析和规范化；
- endpoint -> Tier 分类必须穷尽；
- JSON envelope/schema、error code 和 exit code 映射；
- request profile/hash 的确定性；
- node tree 遍历、depth、subtree hash；
- compact transformer、globalVars 去重；
- prune 计划和 blob 引用计数；
- token redaction。

### 20.2 集成测试

- mock Figma API：200、403、404、429、5xx、timeout、截断 JSON；
- 首次 pull、metadata unchanged、changed、force 的请求次数断言；
- 对每个 local query 安装“网络即失败”的 transport，证明零请求；
- 并发两个 pull 最多产生一次 Tier 1；
- staging 中途崩溃后旧 HEAD 仍可读；
- schema migration 与从 raw blob 重建索引；
- data dir 权限和 token 不落盘；
- CLI stdout 恰好一个 JSON object，stderr 不污染 stdout。

### 20.3 Golden/契约测试

- `fixtures/figma` 只保存脱敏或合成的输入；golden 输出使用 `insta` 的 `.snap` 文件，并放在对应 crate 的 `tests/snapshots`；
- 对脱敏 Figma fixture 固定 compact context、tokens 和 components 输出；JSON 输出使用 `assert_json_snapshot!`；
- snapshot 前统一规范化或 redact 时间戳、signed URL、request ID、绝对路径等易变字段；token 不得进入 snapshot pipeline；
- snapshot 更新必须经 `cargo insta review` 人工确认并提交；CI 使用 `INSTA_UPDATE=no`，不得自动接受新输出；
- 每个 CLI 命令输出通过 `schemas/cli/v1` 校验；
- `schemas/cli/v1` 是 CLI 契约的权威来源；`figstash-cli` crate 内保存字节一致的发布镜像，运行时只嵌入该镜像，仓库测试必须阻止两处内容漂移，确保 crates.io package 不依赖 crate 根目录外的文件；
- 用 feature-parity fixture 覆盖旧项目支持的布局、文字、Paint、effect、component 和 image reference；
- 新官方 schema fixture 加入时，未知字段必须在 raw view 保留。

### 20.4 真实 API 测试

- CI 永不访问真实 Figma API。
- 手工 dogfood 使用专用测试文件和显式命令；执行前记录预估 Tier。
- 每个真实调用的响应经脱敏后才能进入 fixture。
- 不使用企业设计文件作为公开测试资产。

### 20.5 P2/P3 测试

- SVG dependency closure、transform、mask、clip、gradient、image 和 ID collision fixture；
- SVG 本地 raster 与 Figma 单节点基准导出的视觉对比；
- image batch planning 的请求数测试；
- MCP stdio protocol、stdout 纯净度和 tool -> application service 映射测试。

## 21. 影响范围

### 21.1 直接新增

- Rust workspace、CLI 二进制和应用服务；
- Figma REST client 与配额策略；
- 本地 durable snapshot store、索引和迁移；
- 版本化 CLI JSON schema；
- fixture、golden、mock 和端到端测试；
- P2 SVG/visual 与 P3 MCP adapter。

### 21.2 间接影响

- 使用者的 Figma PAT Tier 1/2/3 用量；
- 本地磁盘占用和备份策略；
- Agent prompt/context 大小；
- Figma schema 变更带来的 parser/transformer 兼容；
- 未来 MCP 客户端对 resource 和大结果的支持差异。

### 21.3 明确不变

- Figma Desktop、组织插件策略和席位权限；
- Figma 文件内容与共享权限；
- 官方 Figma MCP 的调用额度；
- 任何其他项目或全局 MCP 配置；
- Figma 设计师工作流。

## 22. 发布、迁移与回滚

- CLI 与 store schema 分别版本化；二者不共享一个隐式版本号。
- 使用 Semifold changeset 管理 workspace package 版本与 changelog；每个 member manifest 显式保存自身 `version`，不继承 `workspace.package.version`。`main` 是 base branch，Semifold 管理独立的 `release` branch；禁止将 release branch 指向 `main`。
- `figstash-core`、`figstash-store`、`figstash-query`、`figstash-figma` 和 `figstash-cli` 均发布到 crates.io；发布只能由 GitHub Actions 中的 Semifold CI 执行。本地和 Agent 环境只允许使用 `cargo publish --dry-run` 与 `cargo package` 验证发布包。
- 全部 package 使用 `AGPL-3.0-only`，共享仓库、README、关键词和 crates.io category 元数据；cargo-deny 只对这五个 workspace package 设置 AGPL 例外，第三方依赖的许可证 allowlist 不变。内部依赖同时声明本地 `path` 与 registry `version`，由 Semifold 在 release branch 上随 package 版本同步更新。
- 首次发布按依赖拓扑执行：先发布 `figstash-core`，再发布依赖它的 library crates，最后发布 `figstash-cli`。在 `figstash-core` 尚未进入 crates.io 前，下游 package 的 Cargo dry-run 预期停在 registry dependency lookup；这不允许绕过 Semifold 执行真实本地发布。
- store migration 必须事务化，并在 destructive migration 前创建 catalog 备份。
- 原始 blob 格式尽量 append-only；新 transformer 可从旧 raw blob 重建派生数据。
- 新 CLI schema 先以 additive 字段演进；删除/改义才升级 `schemaVersion`。
- alpha 阶段发布 crates.io package 和本地 binary，但不自动修改 shell/MCP 配置。
- 回滚到旧 binary 时，若不认识新 store schema，应只读失败并提示兼容版本，不能尝试降级写入。
- `snapshot prune --execute` 是唯一常规物理删除入口；删除后返回被删 snapshot/blob 和可恢复性信息。

## 23. 分阶段交付

### Phase 0：基础契约

- workspace、lint/test/CI；
- JSON envelope/schema、error taxonomy；
- config/auth/URL parser；
- mockable Figma gateway 与 Tier classifier。

### Phase 1：核心 Agent CLI

- snapshot pull/store/index；
- node get/search；
- tokens/components；
- status/list/prune plan；
- quota/doctor；
- parity fixtures 和 CLI E2E。

完成后，项目已经解决主要问题：一次刷新后不限次数本地读取。

### Phase 1.5：完整性与稳定性

- metadata probe、diff、迁移、crash recovery；
- 大文件性能、FTS、并发锁；
- keyring、Windows/Linux 验证；
- feature parity 缺口收敛。

### Phase 2：SVG、图片与视觉回归

- geometry compositor；
- Figma-rendered SVG snapshot 和本地切片；
- image fills、render plan/run；
- local visual compare。

### Phase 3：MCP stdio

- `rmcp` adapter；
- tools/resources；
- stdio E2E 与客户端兼容测试。

## 24. 工程量估算

以一名熟悉 Rust、SQLite 和 API client 的工程师估算，不包含等待 Figma 外部审批：

| 范围 | 工程日 |
| --- | ---: |
| Phase 0 | 2–3 |
| Phase 1 | 8–12 |
| Phase 1.5 | 5–7 |
| Phase 2 | 5–8 |
| Phase 3 | 2–4 |
| 总计 | 22–34 |

核心可用 CLI（Phase 0 + 1）约 10–15 工程日。真实大型文件、复杂 SVG 和跨平台 keyring 是估算中最大的波动项。

## 25. 风险与缓解

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| Figma 限额继续变化 | 策略过时 | endpoint classifier 与 policy 数据分离；结果附来源日期；不伪造 remaining |
| OpenAPI beta/schema drift | 解析失败或字段丢失 | 宽容解析、raw blob、未知字段保留、fixture 更新 |
| 大文件导致磁盘/内存压力 | pull/index 失败 | 流式落盘、zstd、资源上限、staging 原子提交 |
| Agent 误触 `--force` | 浪费 Tier 1 | 命令结果和 help 明确成本；账本；未来可配置月度本地 guard |
| metadata scope 不可用 | 无法低成本探测 | fail closed，要求显式 `--force`；不偷偷调用 Tier 1 |
| 本地账本与官方实际用量不同 | 错误决策 | 始终标注 observed-only，不显示官方剩余值 |
| compact transform 丢语义 | Agent 误解设计 | raw view、golden fixture、原始 payload 永久保留 |
| SVG 结构与 node 非一一对应 | 切片不完整 | fidelity 状态、geometry/remote 双来源、明确 unsupported |
| MCP SDK 演进 | adapter 返工 | MCP 最后接入、固定稳定版、核心不依赖 MCP 类型 |

## 26. 未决但不阻塞核心实现的事项

- crates.io/GitHub 名称实际保留；当前搜索未发现明显 `figstash` crate 冲突，但尚未发布占位。
- Windows 是否列为 v1 正式支持平台；架构保持可移植，当前首要运行环境为 macOS。
- 视觉比较的 perceptual metric 和默认阈值；待 P2 真实 fixture 后通过 ADR 确认。
- MCP tool 粒度与资源 URI；待 Phase 3 基于已稳定的 application service 确认。

以上事项不会改变 Phase 0/1 的数据流、配额安全和 CLI 核心契约。

## 27. 方案验收清单

- [x] 用户给出的范围和优先级已记录。
- [x] Figma 端点、Tier、scope 和 URL 有效期已对照官方资料。
- [x] 旧项目只作为功能基线，不继承不安全/不稳定行为。
- [x] CLI 命令、JSON envelope、错误和网络边界有明确契约。
- [x] 存储、原子性、迁移、并发和失败行为已定义。
- [x] 核心/视觉/MCP 的阶段边界已定义。
- [x] 测试、发布、回滚、风险和工程量已覆盖。
- [x] 当前不存在会阻止 Phase 0/1 开始的开放问题。
