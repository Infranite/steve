# Hub、Node、Client 的职责划分

状态：方向已定（2026-11-05），施工中。协议细节见 [node-protocol.md](node-protocol.md)。

## 0. 思路

**Hub 是大脑，Node 是 harness 的宿主，Client 是 Hub 的窗口。**

- **Hub（steve-server）**：唯一的事实来源，也是唯一的决策者。事项、会话、队列、决策、权限裁决、编排、模型选择与重试、业务 prompt、Steve 工具（定义、授权、执行、审计）、认证、保险箱、渠道、附件和统计都在 Hub。Hub 不跑 harness，也不读写用户的目录；它自己机器上只碰自己的数据目录（DB、blob、保险箱、日志、Web 静态资源、升级文件）。它的职责是连接 Node 和使用 Node。
- **Node**：harness 的宿主，同时负责本机的项目目录、git/worktree、命令执行，以及"本机有哪些 harness、有哪些历史会话"的知识。Node 不持有业务真相，但有一份执行日志：命令去重、正在跑的 attempt、resume 句柄、还没被确认的事件、已完成结果。有了它，断线和重启后才能对账。
- **Client**：只连 Hub，形态是 Web 加一层套壳。同一个工作区里的所有人、所有端看到的是同一份事实，这是多人协作的底座。Client 从不连 Node。

### 两类通信不能混

| | 系统通信 | agent 通信 |
|---|---|---|
| 范围 | Hub ↔ Node、Client ↔ Hub | Node ↔ harness 进程 |
| 协议 | Steve 自己的协议 | ACP、MCP、`claude -p` 的 stream-json，以及以后的其他协议 |
| 词汇 | 会话、回合、attempt、作业、工作区、事件、工具调用、权限请求、用量 | `session/update`、`optionId`、`_meta`、`rawInput`、JSON-RPC id… |
| 谁懂 | Hub、Node、Client | 只有 Node 里对应的驱动 |

agent 协议在 Node 的驱动里终止，所以新写入的数据、运行时的领域接口、SSE 和 Client 里都没有 ACP/MCP 报文。旧库里已有的 ACP 历史行，只通过一个隔离的只读转换器（LegacyEventReader）读取。接入新 harness 或新的 agent 协议，只需要在 Node 上加一个驱动。

### 谁决定、谁执行、谁记录

- **决定**全在 Hub：要不要跑、用哪个模型、失败后换不换、权限批不批、工具能不能用、队列下一条是什么、何时清理 worktree 或合并发布。
- **执行和强制**在 Node：按 Hub 下发的策略（工具范围、可读目录、权限模式、预算、截止时间）运行，并在本机强制执行，包括沙箱、路径根约束、预算到了就终止。Node 可以拒绝超出本机限制的操作，但不做"该不该"的判断。Hub 授权是必要条件，但不能绕过本机限制。
- **记录事实**：Node 只上报事实（发生了什么），Hub 落库并据此推进业务状态。Node 从不直接改事项状态。

## 1. 现状：哪里混在一起了

`devices`、`projects.device_id`、`sessions.device_id`、`bots.device` 都已存在，但值全是 `'hub'`，没有任何路由读取它们。

**agent 协议漏进了系统层**

- 会话事件就是原样存下的 ACP `session/update`（`steve-acp/src/runner.rs`，`add_event(sid, kind, u)`）。Web 端靠 ACP 的 kind、`rawInput`、`_meta` 渲染（`web/src/store.ts`），`steve-hub/src/api/slim.rs` 为此专门做过滤。
- Hub 自己就是 MCP 服务端：地址写死 `127.0.0.1`（`steve-core/src/orchestrator.rs`），只接受回环连接（`steve-mcp/src/lib.rs`）。工具语义因此和 MCP 绑死在一起。
- 本该属于驱动的东西散在 Hub 里：Codex 原始 token 的解析在 `steve-hub/src/wire/acp_stats.rs`；Steve 自己的工具被 runner 硬编码成自动放行。

**harness 宿主有三套，而且混着决策**

- `steve-acp`：agent 会话。队列也在 runner 里（`Host::queue_*`），"队列排空"的语义藏在 `turn_end` 里。
- `steve-core/src/gateway.rs`：Steve 自己的模型调用。模型选择、fallback 和 CLI 参数（`extra: &[String]`）都在这一层。
- `steve-research`：沙箱调研。模型重试、按工作区限流和执行混在一起。
- 另外还有 harness 探测和历史导入：`steve-hub/src/system.rs`、`resume.rs`、`catalog.rs`。

**Hub 直接读写用户目录**

- core 在锁里跑 git、建 worktree、做可写探测：`orchestrator.rs`、`git.rs`、`release.rs`、`cycle.rs`、`upstream.rs`、`lane/*`、`takeover.rs`、`yolo/*`。
- HTTP handler 直接读项目目录和产物：`api/projects.rs`、`api/outputs.rs`。
- 附件拷进 cwd：`steve-core/src/attachments.rs`、`steve-hub/src/files.rs`。
- 流程 cmd 节点、带密钥执行、推送：`steve-flow/src/nodes/cmd.rs`、`steve-modules/src/vault/inject.rs`、`steve-github/src/push.rs`。
- 知识库的文件夹源：`steve-knowledge/src/sources/folder.rs`。

**锁约定**

`steve-core/src/runtime.rs` 规定 runtime 的所有方法都在 core 锁内调用、必须立即返回。远程化之后不能照搬，需要改成三步：锁内形成意图，释放锁后等 Node，结果回来后再入锁按 revision 应用。

**Client**

只支持同源 cookie 浏览器。SSE 帧没有 `id`，掉线后只能全量重取。

## 2. 目标结构

```text
 Client（Web / 套壳）── HTTPS: REST + 可续传 SSE ──┐
                                                  ▼
 ┌──────────────────────────── Hub ─────────────────────────────┐
 │ 事项/会话/队列/决策 · 编排 · 模型选择与重试 · 权限裁决          │
 │ ToolService（工具定义、授权、执行、审计） · 认证 · 保险箱 · 渠道 │
 │ 附件 blob · 统计 · 变更日志 · NodeRouter（选 Node、派单、对账） │
 └──────────────────────────────▲───────────────────────────────┘
                                │ 系统协议：Node 主动外连的一条长连接
                ┌───────────────┴───────────────┐
 ┌──────────────┴─────────────┐   ┌─────────────┴──────────────┐
 │ 本机 Node                   │   │ 远程 Node                   │
 │ 系统协议端 · 执行日志        │   │ 同一个程序                  │
 │ 驱动：ACP / print / 沙箱调研 │   └────────────────────────────┘
 │ 本机 MCP 端点 → tool.call    │
 │ 工作区：目录 · git · exec    │
 └────────────────────────────┘
```

**本机 Node** 在 N0 先嵌在 Hub 进程里，到 N2 改成独立进程（`steve-node` 服务）。只有拆成独立进程，"Hub 重启或升级不打断执行"才对本机也成立；在那之前不承诺这一点。

拆成独立进程还不够，还要满足三条：部署时不能把 Node 和 Hub 同批重启；Node 不放在 Hub 的进程组里；Hub 切换发布目录时，Node 用的二进制和驱动不能跟着消失。Hub 和 Web 的升级要与旧版 Node 的协议保持一个兼容窗口。

拆开后可以删掉的，只是"为了重启 Hub 而先杀掉所有 adapter"这层耦合。下面这些都要保留：Node 自己升级时的 drain；项目发布期间对工作区的独占；Hub 维护时停止派新活；控制面不可达时的授权 lease。发布的 drain（`release.rs`）和 adapter 的 quiesce（`runner.rs`）分开判断。

## 3. Node 上的三个面，共用一套 harness 运行

Node 对外有三个面：**Session**（持久的 harness 上下文）、**Job**（一次性作业），以及 **Workspace / Discovery**（git、exec、文件、扫描、历史）。agent 会话、思考（原 gateway）和调研都是"按某个策略，在某处用某个 harness 跑一次 attempt，产出事件"，所以共用同一份 `RunSpec`，以及同一套进程托管、取消、日志、用量和事件归一。API 层面用不同的类型（TurnSpec / JobSpec）区分它们，不用 `kind + Option<…>` 这种可以随意拼装的写法。三者的安全语义各自独立。

| | 多回合 | 权限 | 工作区 | 调度 |
|---|---|---|---|---|
| session | 是，turn → attempt | 交互式，上报 Hub 裁决 | 绑定 item/项目所在的 Node | 固定 |
| think | 否 | 工具白名单 | 无（Node 的临时目录） | 任一具备该 harness 的 Node |
| research | 可续接 | 只读守卫，Node 即时阻断 | 所需目录必须都在同一个 Node 上 | 固定在目录所在的 Node；续接时固定在原 Node |

**沙箱**不按类型写死，而是 RunSpec 里的一项要求（`required` 或"允许降级到哪些保证"），Node 回报自己实际能提供的能力。达不到要求就明确拒绝。要降级，必须由 Hub 明示并记账。

**attempt** 是 Hub 决定的一次尝试，一个逻辑 turn 可以有多个 attempt。换模型、503 之后重试，都由 Hub 发起新的 attempt；Node 不会自行降级或重提交。每份 RunSpec 只有一个确定的 model/effort。网络重发同一个 attempt 不算重试。Node 只上报"attempt 结束"这一事实，逻辑 turn 是结束还是重试由 Hub 裁定。

## 4. 工具

- Hub 的 **ToolService** 与协议无关，基础工具、模块工具（升级、联系人、知识、保险箱、GitHub、邮件、流程…）和调研的只读工具都在其中。`steve-mcp` 拆成两部分：工具目录和执行留在 Hub，MCP 服务端移到 Node。
- 每次执行都有一份 Hub 签发的工具授权（`execution_ref` + 工具范围）。Node 只把授权范围内的工具暴露给 agent。Hub 在每次 `tool.call` 时重新验证：连接对应的 Node、执行的归属、执行是否仍有效、工具是否在授权内、业务权限。上下文由 Hub 根据自己的数据推导，不信 Node 自报的身份。
- 需要审批的工具返回"已提交审批"并结束这一轮，不让一个请求挂着等人。
- 工具产生的项目副作用（git、exec）由 Hub 生成 Node 操作并记录子操作 id。

harness 自带的工具（Bash、Edit…）不经过 `tool.call`。其中需要交互授权的，通过标准的 `permission.requested` 交给 Hub 裁决。harness 处在已授权模式时可能根本不再询问，因此授权模式的效果必须能探测、能拒绝。不能说"所有文件和 shell 操作都经过 Hub"，也不能把审计描述成全覆盖。

还要区分两种"等待"：harness 在等的权限请求（`permission.requested`），和 Steve 工具在业务上的待审批（`pending_approval`）。这是两种机制，不能互相替代。

## 5. 归属与路由

- 凡是有本机状态的东西都带 Node 归属：项目、item 的实际工作区、会话、调研续接、历史导入、知识库的文件夹源。
- 产物分两种：一种是 Node 上工作区里的文件引用；另一种是交付时上传到 Hub、持久保存的 artifact。后者在 worktree 清理之后仍然可以查看。
- 谁能用一台 Node：工作区权限 ∩ 设备负责人的授权，然后才轮到 Node 本机的根目录限制。没有 agent 执行的操作（浏览目录、同步知识库）也要经过同样的授权，用 `operation_ref` 作为凭据。
- 创建会话时确定 Node：先看 item 已有的工作区，其次是项目的 Node，再次是 bot 的运行设备，最后才是本机 Node。确定后不再改。
- 项目改到别的设备，只影响以后新建的工作区。已有的 item 和 worktree 不会被隐式迁移。
- Node 离线时，绑定在它上面的东西显示"设备离线"，队列保留。在线/离线与接活/暂停/排空/吊销是两个独立的维度。
- 容量按会话、作业、exec 三类分别限制。

## 6. 注册 Node

1. 已登录的用户在 Hub 设备页发起配对，拿到短时有效、只能用一次的配对码。
2. 在新机器上运行 `steve-node pair https://<hub> <码>`。
3. Hub 的确认页显示 Node 公钥指纹，确认后 Node 即上线。之后可以改名、暂停接活、吊销。

细节：

- Node 用私钥证明身份，Hub 存公钥和吊销记录。连接走 TLS，Node 固定 Hub 的身份。
- 本机 Node 自动建立身份，但走同一套授权和绑定。
- Node 上线后上报能力：驱动、harness 版本、是否支持续接/取消/权限/图片、可用模型、沙箱能力、OS、允许的根目录（逻辑 id）、三类并发容量。
- 设备归配对的人，沿用现有的 ownership。

## 7. Client

- 所有 Client 只连 Hub，用同一套 REST + SSE。多端就是 Web 套壳，加载 Hub 的 HTTPS 地址，cookie 照常使用。
- **续传**：Hub 维护一份工作区变更日志，覆盖事项、决策、会话、统计等所有变更。SSE 帧的 `id` 是 epoch+cursor。快照带一致的水位；客户端从水位之后订阅，重连时带上 cursor 只补差量，超出保留窗口则收到 `reset_required`。
- **并发写**：消息、审批、设置修改都带 request_id 和 revision。同一审批由先落库的人生效，其他人看到"已裁决"。
- "同一份数据"指同一工作区内共享事实，工作区之间的隔离不变。个人草稿和阅读位置不共享。

## 8. 这样分开以后自然得到的能力

- **接入任意机器**：装一个 `steve-node` 配对即可，在 NAT 后面或经公网网关都能用；Hub 不需要对外暴露 MCP 或文件。
- **新 harness 或新 agent 协议只改 Node**：加一个驱动即可。
- **三套 harness 宿主合成一套**：进程托管、取消、日志、用量、事件归一只写一遍。
- **工具和 MCP 解耦**：工具只在 Hub 定义、授权、审计一次。换工具协议只是在 Node 上多一种暴露方式。
- **事件 schema 归 Steve 所有**：存储、统计、界面不再随 ACP 变化。
- **多人协作**：同一会话可以多人围观、多人审批，不需要在各端之间同步数据。Hub 仍要负责裁决并发写入，并提供一致的快照和续传（见第 7 节）。
- **调度**：按能力、容量、负责人、在线状态选 Node；think 可以分散到空闲的 Node 上。
- **Hub 升级不打断执行**（从 N2 起）：本机 Node 拆成独立进程后，Hub 重启期间回合照常跑，事件先缓冲，重连后补齐。
- **一台机器的故障被隔离**：Node 离线只影响绑在它上面的工作，其余照常。

## 9. 阶段

每一步都单独发布，单机行为不变。N0 不是单纯搬代码：它定义的接口就是最终语义，不允许跨接口暴露 ACP 事件、MCP 地址或同步的 Host 回调。

| 阶段 | 内容 | 验收 |
|---|---|---|
| N0a | `steve-node-api`：typed ID、RunSpec、事件 schema、错误、能力描述、按面拆分的异步端口（Session/Job/Workspace/Discovery/Blob，以及反向的 EventSink/HubTools/HubFiles）。用 fake Node 测试 core | core 能对着 fake Node 编译和测试；API crate 不依赖 store/core/acp/rmcp |
| N0b | 锁边界改成"意图 → 执行 → 应用" | fake Node 故意慢或失败；两个会话并行互不阻塞；同一会话不重入 |
| N0c | 纵切一个真实会话回合：Hub 认领队列 → workspace.prepare → session.open/turn.start → Steve 事件 → 落库/SSE。包含取消、一次权限请求、经本机 MCP 端点调用 ToolService。队列上移到 Hub，turn/attempt 由 Hub 分配。今天的持久认领、提交与回执（`queued` 的 claim/submitted/receipt）保留，并接到 attempt 上。Web 端对 ACP kind/`rawInput`/`_meta` 的依赖同步切换（`web/src/store.ts`） | runner 不再调用 `queue_*`；新写入不含 ACP 原始报文；旧事件经 LegacyEventReader 正常显示；单机的安全性不比现在差 |
| N0d | think/research 改为 Node 作业；模型 fallback 留在 Hub；去掉 `extra: &[String]` | 图片、流式输出、取消、只读守卫、续接、统计行为不变 |
| N0e | 剩余项目 IO 全部迁到 Workspace 端口：git/lane/release/yolo、projects/outputs、flow cmd、vault exec、push、附件、历史导入。知识文件夹源拆成两部分：Node 负责扫描和读取，Hub 负责配置、调度、对账、索引和可见性。vault 的 File 注入模式要单独决定支持范围：远程 Node 上先禁用，本机保持现状 | 生产路径上，Node 之外不再直接启动 harness，也不再访问用户目录；扫描不完整（中途离线、超限、路径被拒）时不删除索引 |
| N1 | 加固：Node 执行日志（含磁盘额度与高水位停接）、事件 `(node, stream, epoch, seq)` 唯一约束、连续水位 ack、重放、对账、`unknown` 状态的处理 | 在接受前后、启动前后、写事件前后、提交后 ack 前分别注入故障；重发命令/事件/工具调用不产生重复效果；磁盘写满时不丢已编号的事件 |
| N2 | `steve-node` 二进制、WebSocket 传输、配对与吊销、连接 fencing、本机 Node 拆成独立进程、真实路由 | 第二个 Node 用独立的 `STEVE_HOME`，Hub 机器上没有 harness，跑完一个完整事项；重启 Hub 时工作继续；拿别的 Node 的会话冒用会被拒绝 |
| N3 | 界面：配对、选择 Node 和根目录、离线/暂停/unknown 状态、bot 的运行设备 | 浏览器测试；两人同时审批只生效一次 |
| C1 | 变更日志 + 可续传 SSE + 套壳断线恢复 | 断网或手机切后台后恢复，只补差量；超出窗口时只 reset 一次 |

**最大风险**：把"没收到结果"当成"没执行"，导致 prompt、发布、推送、工具副作用被重跑。对策：从 N0 起就有 attempt 身份和 `unknown` 状态，N1 补上持久对账。不确定结果的执行交给 Hub 决定，永远不自动重发。
