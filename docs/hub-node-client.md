# Hub、Node、Client 的职责划分

状态：方向已定（2026-11-05），尚未施工。

## 0. 思路

一句话：**Hub 是大脑，Node 是 harness 的宿主，Client 是 Hub 的窗口。**

- **Hub（steve-server）**：唯一的事实来源和决策者。负责事项、会话、队列、决策、权限裁决、编排、认证、保险箱、渠道、Steve 工具的语义、附件和统计。Hub 不运行任何 harness，也不读写任何项目目录。它的职责是连接 Node、使用 Node。
- **Node**：harness 的宿主。它持有 harness 进程、项目目录、git/worktree，以及本机有哪些 harness 的知识。Node 不存业务状态，也不做业务决策。Hub 所在的机器上跑一个内嵌 Node，和远程 Node 用同一套接口。
- **Client**：只连 Hub。现在的 Web 加一层套壳就够了。所有人、所有端看到的都是 Hub 里的同一份数据，这本身就是多人协作的前提。Client 不会直接连 Node。

**两类通信不能混**：

| | 系统通信 | agent 通信 |
|---|---|---|
| 是什么 | Hub ↔ Node、Client ↔ Hub | Node ↔ harness 进程 |
| 协议 | Steve 自己的协议：会话、回合、事件、作业、工作区操作、工具调用 | ACP、MCP、`claude -p`、以后的其他协议 |
| 谁懂 | Hub、Node、Client | 只有 Node 里对应的驱动 |
| 词汇 | Steve 的概念：回合、工具调用、权限请求、用量 | 各协议自己的概念：`session/update`、`_meta`、`rawInput`… |

agent 协议在 Node 上终止。Hub 和 Client 永远看不到 ACP/MCP 的报文。接入新 harness 或新 agent 协议，只需要在 Node 上加一个驱动，Hub 和 Client 不用改。

## 1. 现状：哪里混在一起了

`devices` 表、`projects.device_id`、`sessions.device_id`、`bots.device` 都已存在，但值全是 `'hub'`，没有任何路由读它们。

**agent 协议漏进了系统层**

- 存库的会话事件就是原样转发的 ACP `session/update`（`steve-acp/src/runner.rs`，`add_event(sid, kind, u)`）。Hub 的 DB、SSE 和 Web 都在解读 ACP 格式，`steve-hub/src/api/slim.rs` 为此专门过滤 `_meta` 和 `rawInput`。
- Hub 自己是 MCP 服务端（`steve-mcp`，`/mcp` 只接受回环地址），地址写死 `http://127.0.0.1:{port}/mcp`（`steve-core/src/orchestrator.rs`）。结果是 Steve 工具的语义和 MCP 这个 agent 协议绑在了一起，并且只能在 Hub 本机使用。

**harness 宿主散在 Hub 各处，有三套各自的实现**

- `steve-acp`：agent 会话。
- `steve-core/src/gateway.rs`：Steve 自己的模型调用，走 `claude -p` 和 `codex`。
- `steve-research`：只读调研，用 `sandbox-exec` 包着 harness 运行。
- 另外还有 harness 探测和历史导入：`steve-hub/src/system.rs`、`resume.rs`、`catalog.rs`、`wire/acp_stats.rs`，以及 `steve-acp/src/codex.rs`、`rollout.rs`。

**Hub 直接读写项目目录**

- 准备和清理 worktree、cwd 可写探测：`orchestrator.rs` 的 `prepare_cwd`、`cwd_usable`、`tidy_worktree`，以及 `git.rs`。
- 合并通道、发布、yolo 里的 git 和 `gh`：`release.rs`、`cycle.rs`、`upstream.rs`、`lane/*`、`takeover.rs`、`yolo/facts.rs`。
- 目录浏览和产物预览：`steve-hub/src/api/projects.rs`、`api/outputs.rs`。
- 流程的 cmd 节点和带密钥执行：`steve-flow/src/nodes/cmd.rs`、`steve-modules/src/vault/inject.rs`。
- 推送：`steve-github/src/push.rs`。

**Client**

只支持一种：和 Hub 同源、带 cookie 的浏览器。SSE 帧没有 `id`，断线后只能全量重取。

## 2. 目标结构

```text
 Client（Web / Web 套壳）── HTTPS: REST + SSE ──┐
                                               ▼
 ┌──────────────────────── Hub ───────────────────────────┐
 │ DB · 事项/会话/队列/决策 · 编排 · 权限裁决 · 认证 · 保险箱 │
 │ 渠道 · Steve 工具（语义与执行） · 附件 · 统计 · Web 静态资源 │
 │                 Node 调度（选择节点、派活、收事件）         │
 └──────────────────────────▲─────────────────────────────┘
                            │ 系统协议：Node 主动外连的一条长连接
              ┌─────────────┴──────────────┐
 ┌────────────┴───────────┐   ┌────────────┴───────────┐
 │ Node "hub"（内嵌）       │   │ Node "mbp"…            │
 │ ┌ 系统协议端 ──────────┐ │   │ 同一个程序              │
 │ │ 会话/作业/工作区/发现 │ │   └────────────────────────┘
 │ └──────────┬──────────┘ │
 │   harness 驱动（agent 协议在这里终止）
 │   ACP 驱动 · print 驱动(claude -p/codex) · 沙箱调研驱动
 │   本机 MCP 端点（把工具调用转成系统协议里的 tool.call）
 │   项目目录 · git/worktree
 └─────────────────────────┘
```

## 3. 系统协议（Hub ↔ Node）

只要高效就行。建议用一条长连接（WebSocket 或 HTTP/2 流），帧用紧凑编码，由 Node 主动外连。协议里只出现 Steve 的概念。

**Hub → Node：调用**

| 面 | 操作 |
|---|---|
| 会话 | `session.open(session_id, harness, cwd, model, effort, mode)` · `turn.run(turn_id, prompt, files)` · `turn.cancel` · `permission.answer(decision_id, option)` · `session.set_mode` · `session.release` · drain（`hold`/`quiesce`/`reopen`） |
| 作业 | `job.run(job_id, kind, harness, prompt, opts)`，kind 有两种：`think` 是原来 gateway 的单次模型调用，`research` 是只读沙箱调研 |
| 工作区 | `ws.prepare`/`ws.usable`/`ws.tidy`、有限的 git 查询与执行集合、`fs.list`/`fs.describe`/`fs.read`、`push`、`exec`（密钥按次下发，不落盘） |
| 发现 | `harness.probe`、`history.list`/`history.import`、`history.locate` |

**Node → Hub：事件与请求**

- 事件，每条带 `(session_id 或 job_id, seq)`：
  - `turn.started`、`message.chunk`、`thought.chunk`
  - `tool.started`、`tool.updated`、`tool.finished`
  - `permission.requested`
  - `usage`
  - `turn.ended`
  - `harness.model`
  - `node.status`

  Node 先在本地缓冲再发，Hub 按 seq 去重并 ack，断线期间的事件在重连后补发。事件格式是 Steve 的 schema，由 Node 里的驱动从 ACP 等格式翻译过来。
- 请求：
  - `tool.call(session_id, name, args)`：agent 调用 Steve 工具时，Node 本机的 MCP 端点收到请求，转成这一条发给 Hub，由 Hub 执行并返回。
  - `tool.catalog`：Node 按 session 向 Hub 拉取工具清单，再通过 MCP 暴露给 agent。
  - `file.fetch(file_id)`：取附件字节。

**由此确定的几件事**

- 队列在 Hub。Node 只执行 Hub 下发的回合，`turn_id` 保证幂等。runner 里现有的 `queue_*` 逻辑上移到 core。
- 权限请求由 Node 上报，裁决在 Hub（用户、YOLO、规则），结果再下发给 Node。
- Steve 自己的思考（gateway）也是 Node 上的作业，Hub 选一个具备该 harness 的 Node 来跑。Hub 机器不用装任何 harness，只要有一个 Node 在线即可。
- 会话的 `device_id` 在创建时确定：先看项目的 `device_id`，其次是 bot 的 `device`，否则用内嵌 Node，之后不再改。
- Node 离线时，绑定在它上面的会话显示"设备离线"，队列保留，等它回来。不迁移到别的 Node，因为 worktree 在那台机器上。
- MCP 的 bearer 只在 Node 内部用于区分本机的 agent 会话。Hub 只认 Node 的连接凭据，以及协议里带的 session_id。

## 4. 注册 Node

1. 在 Hub 设备页点「配对新设备」，拿到一次性配对码。
2. 在新机器上运行 `steve-node pair https://<hub> <配对码>`。
3. 回到 Hub 确认。之后可以改名、暂停接活、吊销。

细节：

- Node 生成密钥，换到一张 Hub 签发、可吊销的设备凭据。Hub 只存它的哈希。
- Node 是单个二进制，配置只有三项：Hub 地址、凭据、允许的项目根目录。
- Node 上线后上报 OS、版本和 harness 清单（即 `harness.probe` 的结果）。`devices.status` 和 `max_sessions` 从这时起真正生效。
- 设备归配对的人，沿用现有的 ownership。

## 5. Client

所有 Client 只连 Hub，用同一套 REST + SSE。多端就是在 Web 外面套壳，加载 Hub 的网址，cookie 照常使用。

对 Hub 的要求只有一条：**SSE 要能续传**。每帧带 `id`，客户端用 `Last-Event-ID` 补差量，超出窗口才全量重取。套壳在手机上断线频繁，这一项收益最大。

Hub 和 Client 之间同样不出现 agent 协议。会话事件统一用第 3 节的 Steve schema，`slim.rs` 里针对 ACP 的过滤届时就可以删掉。

## 6. 这样分开以后自然得到的能力

- **任何机器都能接入**：装一个 `steve-node` 配对即可，NAT 后面或经公网网关都能用，Hub 不需要对外暴露 MCP 或文件。
- **新 harness、新 agent 协议只改 Node**：加一个驱动，Hub 和 Client 不动。
- **三套 harness 宿主合成一套**：会话、思考、调研都是 Node 上的 harness 作业，共用启动、环境、沙箱、用量统计和取消逻辑。
- **Steve 工具和 MCP 解耦**：工具只在 Hub 定义和执行一次，Node 负责把它们暴露成 MCP。以后换别的工具协议，也只是在 Node 上再加一种暴露方式。所有工具调用都经过 Hub，审计、限额和权限只需要做一处。
- **多人协作**：所有人看的是同一份 Hub 数据，同一个会话可以多人围观、多人审批，不需要额外设计同步。
- **调度**：Hub 可以按 harness 清单、`max_sessions`、负责人、在线状态选 Node，思考作业可以分散到空闲的机器上。
- **Hub 升级不打断执行**：Hub 重启时 Node 上的回合继续跑，事件缓冲下来，重连后补发。今天重启 Hub 会杀掉所有 adapter。
- **事件 schema 归 Steve 所有**：存储、统计、界面都不再随 ACP 版本变化而改动。

## 7. 拆分顺序

每一步都能单独发布，单机行为不变。

| 阶段 | 内容 | 验收 |
|---|---|---|
| N0 | 定义 Node 接口（第 3 节的调用、事件和请求，先只做进程内实现）。把第 1 节里 core、hub、flow、github、vault 直接做的 fs/git/process 操作，以及 gateway、research 的 harness 启动，全部移到内嵌 Node 后面。core 持锁时不做 IO | 现有测试和 e2e 不变。Node 实现之外，全仓不再有直接启动 harness、读写项目目录的调用 |
| N1 | 驱动内部把 ACP 翻译成 Steve 事件 schema，老事件由读取端兼容。队列上移到 core。事件带 seq 并 ack。工具调用改走 `tool.call`，Hub 侧的工具执行与 MCP 服务端分离 | runner 不再调用 `queue_*`，DB 里不再写入 ACP 原始报文。断线重连的模拟测试 |
| N2 | `steve-node` 二进制、长连接传输、配对与吊销、Node 本机的 MCP 端点、按 `device_id` 路由、作业选择 Node | 本机用另一个 `STEVE_HOME` 起第二个 Node，在它上面跑完一个完整事项，包括思考作业 |
| N3 | 界面：配对、建项目时选设备、目录浏览经由 Hub 转到 Node、bot 的"运行设备"、离线状态 | 浏览器测试 |
| C1 | SSE 续传。Web 套壳 | 手机套壳断网再恢复后，只补收差量 |
