# Hub、Node、Client 的职责划分

状态：提案（2026-11-05）。描述现状、目标边界和拆分顺序；末尾列出待定事项。

## 1. 现状：三种角色挤在一个进程里

`devices` 表、`projects.device_id`、`sessions.device_id`、`bots.device` 都已存在，但值全部是 `'hub'`，没有任何路由读取它们。Hub 进程同时承担：

| 实际在做的事 | 位置（hub-rs/crates/） | 应属于 |
|---|---|---|
| SQLite、编排、REST/SSE、认证、保险箱、渠道、MCP 工具 | steve-store、steve-core、steve-hub、steve-auth、steve-vault、steve-channels、steve-mcp | Hub |
| 拉起 adapter 进程、ACP stdio、权限请求、取消 | steve-acp `runner.rs`、`rpc.rs` | Node |
| 准备/清理 worktree、cwd 可写探测 | steve-core `orchestrator.rs` `prepare_cwd`/`cwd_usable`/`tidy_worktree`，`git.rs` | Node |
| 合并通道和发布的 git 操作（rebase、merge-tree、patch-id、fetch、`gh`） | steve-core `release.rs`、`cycle.rs`、`upstream.rs`、`lane/*`、`takeover.rs`、`yolo/facts.rs` | Node（Hub 只发起与记录） |
| 项目目录浏览、README、产物预览 | steve-hub `api/projects.rs`、`api/outputs.rs` | Node |
| 流程的 cmd 节点、带密钥执行命令、只读调研 | steve-flow `nodes/cmd.rs`、steve-modules `vault/inject.rs`、steve-research | Node（密钥由 Hub 按次下发） |
| 探测 harness、导入 Claude Code/Codex 历史、找原生记录 | steve-hub `system.rs`、`resume.rs`、`catalog.rs`、`wire/acp_stats.rs`，steve-acp `codex.rs`/`rollout.rs` | Node |
| 自升级、supervisor | steve-hub `upgrades/` | 各机器各自一份 |

客户端一侧只支持一种形态：与 Hub 同源、带 `steve_session` cookie 的浏览器。没有用户 API 令牌、没有客户端身份、SSE 帧没有 `id`、无法续传、没有 CORS；Web 写死相对路径 `/api`。

MCP 地址写死 `http://127.0.0.1:{port}/mcp`（`steve-core/src/orchestrator.rs`），`/mcp` 只接受回环地址（`steve-mcp/src/lib.rs`），所以别的机器上的 agent 根本连不到 Steve 工具。

好消息是执行边界其实已经有了：`steve-acp` 不依赖数据库，Hub→执行器走 `steve_core::AgentRuntime`（`runtime.rs`），执行器→Hub 走 `steve_acp::Host`（`runner.rs`）。黏住的部分是这两个 trait 之外的那些：core 自己在锁里跑 git、探测路径、拷文件，HTTP handler 直接读项目目录。

## 2. 目标边界

```text
 Client（Web / 桌面壳 / 手机壳 / CLI / IM 渠道）
        │  HTTPS：REST + 可续传 SSE；cookie 或 Bearer
        ▼
 ┌──────────────── Hub (steve-server) ────────────────┐
 │ 唯一事实来源：DB、事项/会话/队列/决策、编排、认证、     │
 │ 保险箱、渠道、MCP 工具、附件 blob、统计、Web 静态资源    │
 └───────────────▲───────────────────────────────────┘
                 │  Node 主动外连的一条长连接（WebSocket）
                 │  双向 RPC + 带序号的事件流
 ┌───────────────┴─────────┐   ┌────────────────────┐
 │ Node "hub"（内嵌，同进程） │   │ Node "mbp" / "linux" │ …
 │ adapter 进程、项目目录、   │   │ 同一套协议           │
 │ git/worktree、harness     │   └────────────────────┘
 └─────────────────────────┘
```

三条规则：

1. **Hub 不碰项目目录。** 凡是要读写 `projects.path` 或 session cwd 的，一律经由 Node 接口完成，Hub 自己机器上的也一样。
2. **Node 不存业务状态。** Node 只持有进程、本机文件，以及断线期间没送达的事件缓冲。重装 Node 不会丢任何事项、会话或决策。
3. **Client 只是视图。** 所有客户端用同一套 API 和事件流，不为某一端单独开后门。

Hub 所在机器上的执行能力变成一个**内嵌 Node**：同一个 trait，进程内直接调用。单机部署的行为和性能保持不变；远程 Node 只是把同一个 trait 换成网络实现。

## 3. Hub ↔ Node 协议

协议分三个面，都按 `device_id` 路由。会话创建时取项目的 `projects.device_id`，没有项目时取 `bots.device`，都没有就用 `hub`；一旦确定就写进 `sessions.device_id`，之后不再改。

**A. 会话运行时**：就是现有的 `AgentRuntime` 和 `Host`，外加 drain 控制（`hold`/`quiesce`/`reopen`/`take_cut`/`continue_cut`）。上网络前要先改三处：

- `Host::file` 返回的是本地路径，要改成返回字节，或者一个带短期令牌的拉取地址。附件只在 Node 侧落到 `<cwd>/.steve/attachments/`，`steve-core/src/attachments.rs` 与 `steve-hub/src/files.rs` 里的重复实现一并去掉。
- 队列留在 Hub（它是用户可见的持久状态）。`queue_add`/`queue_take` 目前在 runner 锁里同步调用，要改成 Hub 下发一个"执行这一轮"的指令，带 turn_id，Node 按 turn_id 幂等执行。runner 里的队列逻辑上移到 core。
- 同步写回（`set_status`、`add_event`、`set_usage` 等）改为事件，每条带 `(session_id, seq)`。Node 先存本地缓冲再发；Hub 收到后按 seq 去重并 ack。断线时 adapter 照常跑完当前这一轮，重连后补发。

**B. 工作区操作**：把 core 和 HTTP handler 里直接做的本机事务收拢成一个 `NodeOps` 接口：

- `prepare_cwd`、`cwd_usable`、`tidy_worktree`
- git：给 release/cycle/upstream/lane/takeover/yolo 用的有限查询与执行集合，不开放任意 shell
- fs：`list_dirs`、`describe`、`read_output`、`exists`
- `push`（令牌由 Hub 按次下发）
- `exec`（流程 cmd 节点与带密钥执行；密钥只在调用时下发，Node 不落盘）
- `research_run`

**C. 发现与导入**：harness 探测（runtimes）、可导入的历史会话、原生记录定位、Codex 用量回填。Node 上线时上报一次，之后按需刷新。上报的内容就是 `devices` 行里的 `os`/`version`/`roles`，再加 runtimes。

**MCP**：Node 在本机回环起一个 MCP 代理，agent 仍然连 `127.0.0.1`，代理把请求连同 session bearer 经长连接转给 Hub。这样 Hub 的 `/mcp` 继续只对回环开放，不用暴露到公网，`McpEndpoint` 的地址改由 Node 提供。

## 4. 注册 Node 的最小成本

用户要做的只有三步：

1. 在 Hub「设备」页点「配对新设备」，拿到一个一次性配对码（10 分钟有效）。
2. 在新机器上运行 `steve-node pair https://<hub> <配对码>`。
3. 在 Hub 上确认；设备随即出现，可以改名、暂停接活、吊销。

具体过程：

- Node 在本机生成密钥对，用配对码换取 Hub 签发的设备凭据。凭据是一个可吊销的设备令牌，Hub 只存哈希，`devices` 表里加 `credential_hash` 和 `revoked` 两列。
- 之后 Node 主动外连，所以 NAT 后面、经公网网关都能用。
- Node 是单个二进制，配置只有 Hub 地址、凭据和允许的项目根目录。不需要数据库，不需要 OIDC，也不需要保险箱。
- `devices.status`（暂停接活）和 `max_sessions` 从这一步开始真正生效：Hub 派活前先检查。
- 负责人沿用 ownership：设备归配对的人所有（参见"一切挂到人"）。

## 5. 多端 Client

按成本从低到高排：

1. **桌面/手机壳直接加载 Hub 的 URL**（Tauri/Capacitor 之类）。和浏览器同源，cookie 照用，几乎零改动。推荐先做这一步。
2. **Bearer 令牌**：`lookup_user` 在 cookie 之外再接受 `Authorization: Bearer`。令牌有两种来源：个人令牌（CLI、脚本）和客户端设备令牌（走和 Node 一样的配对流程，`roles` 标为 `客户端`）。工作区仍用 `x-steve-ws` 指定。
3. **可续传 SSE**：每帧带 `id`（全局单调序号），Hub 保留最近 N 条（或者落在 change log 里），客户端带 `Last-Event-ID` 重连只补差量。超出窗口才发 `workspace` 全量重取。手机端频繁断线时这一项收益最大。
4. **独立前端源**：Web 加可配置的 base URL，Hub 加 CORS 白名单（复用 `STEVE_PUBLIC_URLS`）。只有真的要把前端从 Hub 拆出去时才需要。

## 6. 拆分顺序

每一步都可以单独发布，并且不改变单机行为：

| 阶段 | 内容 | 验收 |
|---|---|---|
| N0 | 在 steve-core 定义 `NodeOps`，把第 1 节里所有直接的 fs/git/process 调用迁到内嵌实现后面；core 持锁时不做 IO | 现有测试和 e2e 不变；全仓直接 `std::process`/`std::fs` 的调用只剩 Node 实现、blob 存储、知识源、升级 |
| N1 | 第 3 节 A 面的三处改造：附件字节化、队列上移、事件带 seq 并 ack | runner 不再调用 `queue_*`；断开再重连的模拟测试 |
| N2 | `steve-node` 二进制、WebSocket 传输、配对与吊销、MCP 代理、按 `device_id` 路由 | 本机用另一个 `STEVE_HOME` 起第二个 Node，在它的项目上跑完整事项 |
| N3 | UI：配对、建项目时选设备、目录浏览经由 Node、bot 的"运行设备"字段生效、离线状态 | 浏览器测试 |
| C1 | Bearer 令牌 + SSE 续传 | CLI 能列出事项、发消息、跟随会话 |

## 7. 待定

1. **队列归属**：建议放 Hub，Node 只执行"这一轮"。另一种做法是把队列放到 Node，改动更少，但用户可见状态就分散到了多台机器。
2. **Steve 自己的模型调用**（`steve-core/src/gateway.rs` 用 `claude -p`/`codex`）：建议先留在 Hub 机器，后面再考虑声明了 `网关` 角色的 Node。
3. **传输方式**：建议 WebSocket + JSON（与 ACP/MCP 的 JSON-RPC 风格一致，过网关简单）。gRPC 性能更好，但部署和调试成本更高。
4. **Node 离线时，绑在它上面的会话**：建议标为"设备离线"并保留队列，等它回来；不自动迁到别的 Node，因为 worktree 在那台机器上。
