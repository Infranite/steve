# Hub ↔ Node 系统协议

配合 [hub-node-client.md](hub-node-client.md) 阅读。这里定义逻辑 schema；字节层的编码可替换。协议里只出现 Steve 的概念，不出现任何 ACP、MCP 或 CLI 参数。

## 1. 传输与编码

- Node 主动发起一条 TLS WebSocket 长连接，所有通信走这一条连接：双向的请求/响应，加上多路事件流。
- 编码先用 JSON，N0/N1 都用 JSON 做验证。如果测量证明有必要，N2 起换成 Protobuf envelope。每条连接在 `hello` 时协商一种编码，保留 JSON 用于调试。
- 首个 `hello` 固定用 JSON 编码，协商完成后再切换到选定的编码。
- 大小上限一律按编码后整条消息的字节数计算。初始值：控制消息 64 KiB；事件批 256 KiB，或每 20 ms 刷新一次；blob 分块 256 KiB。单个事件必须能放进一个批次。超出的文本、图片、diff 改为 artifact 引用。
  - 这里的"控制消息"是协议自己的概念，不是 WebSocket 的 control frame，后者最多 125 字节。
- 优先级：控制消息、权限回复、cancel、ack 优先于普通事件和 blob，避免大附件挡住停止命令。
- 流控：每个 Node 一个总窗口，每条执行流再按字节分配 credit，多个会话之间公平轮询。

### Envelope

```text
protocol: {major, minor}
msg_type
request_id            每次请求唯一
correlation_id?       响应对应哪个请求
trace_id
connection_epoch
execution_ref?        session+turn+attempt | job+attempt
operation_id?         同一操作重试时保持不变
deadline_at?
payload
```

- Node 的身份由已认证的连接确定，载荷里的字段不能覆盖它。
- 业务上的 workspace 由 Hub 根据绑定关系查出，不信 Node 自报。

## 2. 消息

| 方向 | 消息 | 要点 |
|---|---|---|
| 双向 | `hello` / `welcome` / `heartbeat` / `goodbye` / `error` | 版本区间、能力、帧上限、connection_epoch、恢复水位 |
| Hub→Node | `session.open` / `configure` / `inspect` / `release` | 参数是 workspace_ref、harness、model、effort、mode、工具授权；返回不透明的 resume handle。`release` 只释放空闲的 harness 进程，不关闭 Steve 会话 |
| Hub→Node | `turn.start` / `cancel` / `inspect` | 带 session_id、turn_id、attempt_id、prompt parts、files、limits。每个会话同时最多一个活动 attempt；忙时返回 `busy`，Node 不自己排业务队列 |
| Hub→Node | `job.start` / `cancel` / `inspect` | kind 为 `think` 或 `research`；带 attempt_id、limits、可读范围、continuation |
| Hub→Node | `permission.resolve` | 绑定 `permission_id + attempt_id + action_digest + decision_revision`，带 Steve 决定（`allow_once` / `reject`）。Node 只对还在等待的原请求生效，由 Node 映射成 ACP optionId。过期或不匹配时返回 `stale`，Hub 据此关闭卡片。同样的答复重复发送是幂等的；冲突的答复被拒绝。参数、目标 HEAD 或资源一变，旧授权即失效 |
| Hub→Node | `workspace.prepare` / `inspect` / `tidy` | 工作区句柄 + revision |
| Hub→Node | `git.inspect` / `git.apply` | 粒度较粗的事实查询，一次返回 refs、status、merge-base、冲突，并带上观察到的 revision。写操作带前置条件（`expected_head`、`expected_ref`），apply 时再校验一次。不开放任意 git argv。Node 只回事实和证据，不做业务决定：不选 merge 还是 rebase，不自动解决冲突，不决定删分支或发布版本。多个 worktree 共享一个 repo 时，写操作按 repo 协调 |
| Hub→Node | `exec.start` / `cancel` / `inspect` | 流程 cmd 节点、带密钥执行、项目发布脚本。密钥走短期投递，不进入持久化的命令体 |
| Hub→Node | `fs.list` / `describe` / `read` | 分页、范围、大小上限。Node 负责 realpath、symlink 和根目录约束，每次打开文件时都重新检查 |
| Hub→Node | `tree.scan` / `scan.page` | 知识库文件夹源用。manifest 是带版本的扫描结果，不是文件系统的原子快照。`read` 带期望的版本或 hash，文件变了就返回 `changed`。只有声明了完整、且 source revision 仍然匹配的扫描，才能拿来对账"文件已删除" |
| Hub→Node | `harness.probe`、`history.list` / `import` / `locate` | `import` 返回标准事件，不在 Node 上建 Hub 的业务行 |
| Hub→Node | `node.drain` / `node.resume` | "停止接新活"和"取消在跑的任务"是两回事 |
| Node→Hub | `events.batch` / `execution.snapshot` / `node.status` | 只报事实。快照包含活动执行、等待中的权限和终态回执 |
| Hub→Node | `events.ack` / `replay.request` / `flow.credit` | 落库之后才 ack 连续水位；发现缺口时请求重放 |
| Node→Hub | `tool.catalog` / `tool.call` / `tool.inspect` / `tool.cancel` | 按执行授权；`tool_call_id` 保持稳定 |
| 双向 | `blob.begin` / `chunk` / `finish` / `abort` | transfer_id、hash、size、offset。双向都支持：Node 拉附件，Node 把要长期保存的产物上传到 Hub。finish 时校验 hash 后才算提交。传输的恢复和任务的恢复相互独立。密钥不走 blob，用单独的、不持久化的投递通道 |

## 3. 状态机

```text
Hub 持久队列 → dispatch outbox → Node 已接受 → running ⇄ waiting(权限/工具)
                                                  ↓
                         succeeded | failed | cancelled | timed_out | interrupted
                                                  或 unknown（结果不确定）
                                                  ↓
                                Hub 提交事件 + 投影 + 决定下一次派单
```

- **Session**：`unopened → opening → ready ⇄ active → releasing → released`，从 `released` 可以重新 `opening`。`closed` 表示业务上已关闭，不再派单。`offline` 是连通性的另一个维度，不改变运行状态。
- **Attempt**（Node 直接报告的执行事实）：`accepted → running ⇄ waiting → succeeded | failed | cancelled | timed_out | interrupted`。
  - waiting 期间待处理的请求是一个集合，不是单个布尔值。
  - cancel 只是一个意图，最终以 Node 报回的终态为准，正常完成可能先到。cancel 不等于回滚。
- **Turn**（Hub 的逻辑回合）：`queued → dispatching → attempt… → 结束`。一个 turn 可以有多个 attempt；`attempt.ended` 不代表 turn 已经结束，由 Hub 裁定是结束还是再发一个 attempt。Hub 还要判断队列是否已排空，再推进事项。今天藏在 `turn_end` 里的"排空"语义改归 Hub。
- **Job**：attempt 的状态机与 Turn 相同，结果放在 `job.result` 里，以引用形式给出。
- **Workspace / exec 操作**：`accepted → running → committed | failed`。有外部副作用的操作，超时或断线不等于没有发生。
- **`unknown`** 是观察和恢复时的状态，不是终态，也不保证进程已经停止。Hub 不能仅凭 `unknown` 就释放工作区或派发第二个 attempt。

## 4. 幂等、恢复与事务边界

1. Hub 在同一事务里认领消息（按 message_id 集合，不按文本去重）、分配 turn 和 attempt、写 dispatch outbox，提交之后再发命令。
2. Node 先把 `operation_id` 和请求 hash 写进 inbox，再启动进程。
   - 同一个键、同一个 hash：返回已有的状态或结果。
   - 同一个键、不同的 hash：返回 `conflict`。
   - 终态保留 tombstone。
   - 每条命令带固定的 `issued_at` 和 `accept_before`，重试时不更新。过了 `accept_before` 的命令一律返回 `stale`，所以 tombstone 清理之后，旧命令也不会被当成新命令执行。
   - 除了命令去重，执行身份也唯一：同一个 attempt 换了 operation_id，或者同一个 operation_id 换了 payload，都不会启动第二份执行。
3. Node 的每个事实先写入执行日志再发送。Hub 对 `(node_id, stream_id, stream_epoch, seq)` 加唯一约束；事件、统计、状态投影和后续 outbox 在同一事务提交，然后 ack 连续水位。事件有没有送到某个 Client，不影响 ack。
4. 启动进程和写日志做不到原子，所以不宣称端到端 exactly-once。无法确定结果的执行标为 `unknown`，交给 Hub 决定，永远不自动重发原 prompt。
5. 重连顺序：认证 → 协商新的 connection_epoch → 交换水位和执行快照 → 补发事件 → 恢复派单。
   - 新连接会 fence 掉同一 Node 旧连接的控制权。有多个新连接时，由 Hub 发放单调递增的 fencing generation 决定谁有效。
   - fencing 只处理连接的归属，不会让正在跑的执行失效：重连后，旧 attempt 的事件照常接收。
   - Node 丢失日志后，开一个新的 stream epoch，不复用旧的 seq。
6. Hub 重启不会取消已经授权、正在跑的计算。需要新权限或新工具的工作就等 Hub 回来。Node 离线还能继续跑多久，由 Hub 事先下发的 lease 和 limits 决定。
7. Hub 从备份恢复数据库时，Node 可能已经清掉了被 ack 的事件。为此要有 hub incarnation 和日志保留窗口；超出窗口时报告缺口，不能假装无损。
8. **执行日志的磁盘规则**：日志有总额度。接近高水位时停止接新活，并向 Hub 报告；控制消息和终态始终留有预留空间。还没分配 seq 的 token 增量可以先在内存里合并，但已编号的事件绝不丢弃。正在跑的执行输出达到上限时，按预先设定的规则安全终止或暂停。cancel 和权限消息的额度与数据流分开，避免互相等待。

## 5. 版本与错误

- **版本协商**：`hello` 带上协议的 min/max、事件 schema 版本、能力、编码和帧上限，`welcome` 从中选定交集。major 不兼容就拒绝连接；minor 只做可选的新增。未知的必需能力只拒绝对应的操作。未知的枚举值永远不能映射成 allow。
- **错误**：字段为 `code, retry_class, operation_id, execution_state, safe_detail`。错误码区分 `offline`、`busy`、`unsupported`、`permission_denied`、`expired`、`conflict`、`resource_exhausted`、`outcome_unknown`。
- **超时有四种**，各自独立，网络重发不会重新计时：
  - RPC 等待时限：只表示没等到回复。
  - 审批有效期。
  - 执行 deadline：由执行者落实，杀掉进程组并回报实际终态。
  - 离线 lease。

## 6. Steve 事件 schema

```json
{
  "schema_version": 1,
  "event_id": "ev_…",
  "source": {"kind": "node", "node_id": "nd_…", "driver": "claude-code", "driver_version": "…"},
  "stream": {"id": "session:s_…", "epoch": "se_…", "seq": "42"},
  "execution": {"session_id": "s_…", "turn_id": "t_…", "attempt_id": "a_…"},
  "type": "message.delta",
  "occurred_at": "…",
  "payload": {"message_id": "m_…", "block_id": "b_…", "role": "assistant", "format": "text", "offset": 0, "text": "…"}
}
```

| 事件族 | 关键字段 |
|---|---|
| `session.opened` / `configured` / `released` | 请求的与实际生效的 model、effort、mode |
| `turn.started` / `ended`、`job.started` / `ended` | execution ids；ended 带 outcome、stop_reason（Steve 枚举）、safe error、result_ref、最后的事件水位 |
| `message.started` / `delta` / `completed` | message_id、block_id、role、format、offset；delta 与 snapshot 要区分清楚 |
| `thought.delta` | 只放驱动本来就公开的内容 |
| `tool.started` / `updated` / `finished` | invocation_id、parent?、origin（steve/harness/external）、name、category（read/edit/exec/search/fetch/steve/other）、状态、结构化 action、结果 parts、artifact refs |
| `permission.requested` / `resolved` / `expired` | permission_id、attempt_id、invocation_id、结构化 action（exec 带 command 和网络意图，写文件带规范化后的路径和变更摘要）、可选的 Steve 动作、expires_at。无法归一的操作标为 unknown，走明确的策略 |
| `usage.reported` | report_id、model、各类 token 数、context used/limit、cost（decimal + 币种）、scope、`delta` / `cumulative` / `final`、meter_epoch。不知道的值为 null，不填 0 |
| `workspace.changed`、`artifact.available` | workspace_ref、revision、artifact_id、mime、size、hash。不带本机绝对路径 |
| `diagnostic`、`capability.changed` | 标准的 severity/code/text。不透传未知的原始报文 |

**用量记账规则**（用 reducer fixture 验证）：

- `main` 和 `all` 两个 scope 不能直接相加。
- `delta` 和 `final` 不能重复记账。
- 失败 attempt 的用量不能丢。
- 后台用量（harness 在我们的回合之外自己做的工作）没有当前 attempt，事件的 subject 填 session，不硬挂到最近一个 turn 上。
- meter_epoch 变化时，累计值重新从零开始计。

序号有三种，互不混用：Node 流的 seq、会话展示用的序号、工作区变更 cursor。浏览器端的 JSON 里，u64 一律写成十进制字符串。

**旧事件兼容**

- 历史行保持不动。新写入的行标 `format=steve.v1`，旧行认定为 `legacy.acp`。
- 设一个隔离的只读 `LegacyEventReader`，把旧行转成 Steve schema。它只在读取路径上使用，不参与 Node 协议。
- 旧数据缺少的边界标为 inferred，不捏造。
- `slim.rs` 里针对 ACP 的过滤可以删掉，但大小控制、图片引用、脱敏和可见性投影要保留，这些是 Steve API 自己的职责。

## 7. tool.call

请求：`{tool_call_id, execution_ref, catalog_revision, name, args, deadline_at, causation_id}`。

Hub 的处理顺序：

1. 校验 Node 连接与执行的绑定、执行是否有效、工具授权，以及 Hub 自己保存的 bot/workspace/actor。不能把 agent 提升为工作区 owner。
2. 校验 args 的 schema、资源范围、预算和业务权限。
3. 写调用账本，唯一键为 `(execution_ref, tool_call_id)`，`args_digest` 是这一行的内容。
   - 同一个键、同一个 digest：返回已有的结果或"进行中"回执。
   - 同一个键、不同的 digest：返回 `conflict`。
   - 结果、审批和 outbox 都挂在这一行上。这样提交之后即使响应丢了，重发也不会再执行一遍。
   - 业务写入和审计在同一事务里完成；外部操作走 durable outbox。
4. 需要审批时返回 `pending_approval + operation_id`。
5. 执行。有项目副作用的生成 Node 操作，记录子 operation_id。持有 core 锁时不等 Node。
6. 返回 Steve ToolResult，由 Node 包装成 MCP 的 content/isError 交给 agent。

审计字段包括：node、principal、workspace、execution_ref、tool_call_id、工具及版本、脱敏后的 args 及其 hash、授权范围、决定与审批人、各个时间点、子操作、结果摘要、外部回执。密钥不进入审计、SSE、错误信息和 Node 的持久化命令体。

**超时与 `unknown`** 沿用第 3、4 节的通用规则：取消不等于回滚；外部效果不确定时返回 `outcome_unknown`，先查外部回执，不让 agent 换个 call id 盲目重试。Hub 崩溃后从账本和 outbox 恢复。

**密钥注入的三种方式**：env、stdin 和 File。"不进日志"并不等于"不落盘"，File 模式会写一个临时文件（`vault/inject.rs`）。在远程 Node 上，先禁用这种无法保证不落盘的模式，本机 Node 保持现状。
