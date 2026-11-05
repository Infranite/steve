# 交付队列方案（ST-15）

状态：方案，2026-11-01。其中“同一目标的合并排队、并发时成批合并”已由合并通道实现（`hub-rs/crates/steve-core/src/lane/`），其余部分仍未实现。

## 1. 背景与目标

现在一件事项验收通过后，就为它单独走一次交付：

- **发布**：`await_release` 让 Bot 调 `steve_publish` → 开一张“发布”审批卡 → 批准后 `rebase_for_release` 把分支 rebase 到 main → `start_release` 跑 `ops/release.sh <branch> <item>`。改到 Hub 时，这个脚本会 drain、重启 Hub、做健康检查（`hub-rs/crates/steve-core/src/release.rs`、`orchestrator.rs` 的 `tool_publish` / `await_release`）。
- **PR（MR）**：在关联了 GitHub 的项目里，`on_accepted` 会为每件事项开一张 `gh_pr_open` 卡；检查全部通过后再为每个 PR 开一张 `gh_pr_merge` 卡（`hub-rs/crates/steve-github/src/accept.rs`）。

带来的问题：

1. 一天十几件小改动，就会有十几次构建、drain 和重启。每次重启都会打断正在进行的回合，Steve 也要停大约一分钟。
2. 每件事项都要单独审批一次发布或 PR，待处理里全是同一类卡。
3. 同时做的几件事项会互相抢发布锁（“另一个发布正在进行”），后面的直接失败，只能手动“继续”。
4. 先合进 main 的分支会让后面的分支落后，于是出现“不能快进合并到 main，需要先 rebase”这类失败 [K1]。

目标：把高频操作，或者短时间内针对同一项目的操作，先放进**交付队列**，按批次一起处理。一个批次只审批一次、构建一次、重启一次；一个批次合入的 PR 也一次审批。原来“一件一发”的路径继续保留，作为直通和紧急通道。

不在本方案范围：跨项目的批次（一个批次只属于一个项目）；不经批准就对外生效（自动审批只作为可选项，见 §10）。

## 2. 核心概念

| 概念 | 说明 |
| --- | --- |
| 交付动作 `kind` | `release`（运行项目的发布脚本）、`pr_open`（推分支并开 PR）、`pr_merge`（合并 PR）。可以扩展。 |
| 通道 lane | `(工作空间, 项目, kind)`。每条通道有一份策略，同一时刻最多有一个“收集中”的批次和一个“执行中”的批次。 |
| 条目 entry | 一件事项的一次交付请求：事项、分支、入队时的 head、kind。 |
| 批次 batch | 一条通道上一起处理的若干条目，编号 `DQ-n`，有自己的状态、触发原因、审批卡和结果。 |
| 策略 policy | 通道的规则：`direct`（现状：一件一发）或 `queue`，以及触发条件。 |

## 3. 数据模型（Hub 持久化）

新增迁移 `hub-rs/crates/steve-store/migrations/013.sql`。只建新表，用 `IF NOT EXISTS`，不改已有的表，和 004–012 的约定一致：TS Hub 打开这个库不受影响，回滚到 TS 版本仍然可用。

```sql
-- 每条通道的策略；没有记录 = direct（现状）
CREATE TABLE IF NOT EXISTS delivery_policies (
  workspace_id TEXT NOT NULL,
  project_id   TEXT NOT NULL,
  kind         TEXT NOT NULL,               -- release | pr_open | pr_merge
  mode         TEXT NOT NULL DEFAULT 'direct', -- direct | queue
  quiet_s      INTEGER,                     -- 静默期：最后一次入队后多少秒没有新条目就触发
  max_count    INTEGER,                     -- 条目数达到多少就触发
  max_age_s    INTEGER,                     -- 最早的条目等了多久就触发
  windows      TEXT NOT NULL DEFAULT '[]',  -- 定时窗口，JSON：["12:00","18:00"]，本地时间
  min_gap_s    INTEGER,                     -- 两次执行之间至少间隔多久（保证频率）
  auto_approve INTEGER NOT NULL DEFAULT 0,  -- 1 = 批次不再单独审批（默认关）
  paused       INTEGER NOT NULL DEFAULT 0,  -- 暂停自动触发，只能手动交付
  updated      INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, project_id, kind)
);

CREATE TABLE IF NOT EXISTS delivery_batches (
  id           TEXT PRIMARY KEY,            -- dqb_…
  workspace_id TEXT NOT NULL,
  no           INTEGER NOT NULL,            -- 显示为 DQ-<no>
  project_id   TEXT NOT NULL,
  kind         TEXT NOT NULL,
  status       TEXT NOT NULL,               -- 见 §4.2
  trigger      TEXT,                        -- manual | quiet | count | age | window
  due_at       INTEGER,                     -- 自动触发的预计时间（界面倒计时用；重启后照算）
  branch       TEXT,                        -- 集成分支 steve/deliver/DQ-<no>
  decision_id  TEXT,                        -- 本批次的审批卡
  attempt      INTEGER NOT NULL DEFAULT 0,
  result       TEXT NOT NULL DEFAULT '{}',  -- version、kind(hub/web/hub+web)、log、error、各条目结果
  created      INTEGER NOT NULL,
  sealed       INTEGER,                     -- 冻结时间：之后的新条目进下一批
  started      INTEGER,
  finished     INTEGER
);
CREATE INDEX IF NOT EXISTS delivery_batches_lane ON delivery_batches (workspace_id, project_id, kind, status);

CREATE TABLE IF NOT EXISTS delivery_entries (
  id          TEXT PRIMARY KEY,             -- dqe_…
  batch_id    TEXT NOT NULL REFERENCES delivery_batches(id),
  item_id     TEXT NOT NULL,
  kind        TEXT NOT NULL,
  branch      TEXT,
  head_sha    TEXT,                         -- 入队或更新时分支的 head
  subject_id  TEXT,                         -- pr_merge：PR 的 id（ghp_…）
  status      TEXT NOT NULL,                -- 见 §4.1
  pos         INTEGER NOT NULL,             -- 批次内的顺序（集成和合并都按这个顺序）
  note        TEXT NOT NULL DEFAULT '',     -- 被退回或失败的原因
  enqueued    INTEGER NOT NULL,
  updated     INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS delivery_entries_live
  ON delivery_entries (item_id, kind) WHERE status IN ('queued','integrated','running');
```

说明：

- 每件事项在每个 kind 下最多只有一个“活着”的条目（唯一索引）。这是“合并同类项”的基础，见 §4.3。
- 运行时状态（`st.releasing`）照旧放在内存里；批次状态放在库里，重启后据此恢复，见 §4.6。
- `releases/last.json` 里加上 `batch` 和 `items`，用来把脚本的结果对回批次，见 §8。

## 4. 生命周期

### 4.1 条目状态

```
queued ──冻结并集成成功──▶ integrated ──批次开始执行──▶ running ──▶ delivered
  │                         │                            └──▶ failed（批次失败，回到 queued 或交回 Bot，见 §4.5）
  ├──撤回──▶ withdrawn       └──rebase 冲突/工作区不干净──▶ ejected（交回 Bot 处理）
  └──事项又有新提交──▶ 更新 head_sha，仍是 queued
```

### 4.2 批次状态

```
collecting（收集中，可以继续入队）
   │ 触发：手动 / 静默期 / 数量 / 等待时长 / 定时窗口
   ▼
sealing（已冻结，正在集成：逐个 rebase 到集成分支）
   │ 至少一个条目集成成功
   ▼
awaiting（等你批准；auto_approve 时跳过）
   │ 批准                       │ 拒绝 → cancelled，条目回到下一批的 queued
   ▼
running（脚本在跑：构建 → drain → 重启 → 健康检查）
   ├──▶ succeeded（全部交付）
   ├──▶ partial（hub+web 时 Hub 成功、前端失败；或 PR 批次里部分合入）
   └──▶ failed（未生效，main 已回到原处）
```

批次冻结时，通道上会立即新建一个 `collecting` 批次，所以执行期间入队的条目自动进入下一批。

### 4.3 入队与合并同类项

**入队时机**（只对 `mode=queue` 的通道生效；`direct` 完全保持现状）：

- **发布**：现在是验收通过后 `await_release` 让 Bot 去调 `steve_publish`。改为验收通过后由 Hub 直接入队，不再经过 Bot。事项 `now` 显示“已入队 · DQ-3，等批次发布”，会话结束这一回合（和请求审批一样做 `Mark`，不算“停下来了”）。如果 Bot 仍然调了 `steve_publish`，同样只做入队，并回复“已入队 DQ-3”。
- **开 PR**：`offer_pr` 不再开 `gh_pr_open` 卡，改为入队 `pr_open`。
- **合并 PR**：`ask_merges` 发现 PR 已就绪（检查通过、可合并、没有被要求修改）时，不再开 `gh_pr_merge` 卡，改为入队 `pr_merge`。
- **手动**：事项详情页的“加入交付队列”；或者在对话里对 Steve 说“这个先别发，攒一攒”。

**合并同类项**：

1. **同一件事项重复入队**：例如返工后再次验收、Bot 补了提交，此时不新增条目，只更新原条目的 `head_sha` 和 `updated`。如果原条目所在的批次已经冻结，就把它移到下一批，并在旧批次上标记“已被更新的版本取代”。
2. **同一项目的多件事项**：自然落在同一个 `collecting` 批次里，执行时合成**一次**集成、**一次**构建、**一次**重启，只开一张审批卡。
3. **Hub 和前端**：不需要另外拆通道。`release.sh` 已经会按改动范围决定是 `hub`、`web` 还是 `hub+web`。整批只改了前端时就不重启（见 §11 问题 9）。
4. **PR**：`pr_open` 批次默认“多个 PR 一次开”（逐个推送、逐个开 PR，只审批一次）；另一种做法是合成一个 PR，见 §11 问题 7。`pr_merge` 批次按顺序合并多个已就绪的 PR。

### 4.4 冻结与集成（出队）

触发后批次进入 `sealing`。Hub 在 `~/.steve/worktrees/DQ-<no>` 从最新的 main 建出集成分支 `steve/deliver/DQ-<no>`，然后按 `pos` 逐个处理条目：

1. 在**事项自己的 worktree** 里把事项分支 rebase 到集成分支当前的 head。这里复用 `git::rebase_onto`，和现在 `rebase_for_release` 对 main 做的事一样。
2. 把集成分支快进到这个事项分支。
3. 工作区不干净、rebase 有冲突或出错时，这个条目标记为 `ejected`，写明原因并交回 Bot（消息内容同现在的 `rebase_for_release`，只是把 main 换成“最新的 main”）。其余条目继续处理。

这样做的好处：

- 每个事项分支最后都是集成分支的祖先。发布成功、main 快进以后，`tidy_branch` 的“已合入 main”判断仍然成立，worktree 和分支照常回收。
- `release.sh` 的契约不变：仍然是“把一个分支快进合并到 main 再发布”。分支名换成集成分支即可。
- 后进入的分支自动基于前面的分支，不会再出现“前一个发布把 main 推走了，后一个不能快进”的问题。

所有条目都被退回时，批次直接结束为 `cancelled`，不开审批卡。

`pr_open` 和 `pr_merge` 批次不需要集成分支，这一步换成“逐个检查分支领先 base / PR 仍可合并”。

### 4.5 执行、失败与重试

**执行**（release）：批准后调用 `start_release` 的批次版本：`release.sh steve/deliver/DQ-3 ST-12 ST-14 ST-15`，并通过环境变量传入 `STEVE_BATCH=dqb_…`。批次和条目都进入 `running`，每件事项的 `now` 显示“正在发布（DQ-3）”。其余流程不变：drain 等正在进行的回合结束，再重启。

**结果**（`report_release` 读 `last.json`）：

| 结果 | 批次 | 条目 / 事项 |
| --- | --- | --- |
| 成功 | `succeeded`，记下版本 | 全部 `delivered`；每件事项 `finish(.., "已发布 <版本>（DQ-3）")` |
| Hub 成功、前端失败 | `partial` | 事项保持未完成，`now`=“Hub 已发布，前端待重试”；批次卡上提供“重试前端”（`release.sh --web main`） |
| 失败（构建、健康检查、回滚） | `failed`；脚本已把 main 重置回原处 | 条目回到 `queued`，**不**交回 Bot；开一张 `fail` 卡，选项见下 |
| 脚本没有报告结果 | `failed`，注明“不确定现在是哪个版本” | 同上（沿用现有的 `NO_RESULT` / `pending_abandoned` 逻辑） |

**失败卡的选项**：

- **重试整批**：`attempt+1`，从集成这一步重新开始，因为 main 可能已经变了。
- **拆开逐个发**：把批次拆成每件一个的小批次，按顺序执行，找出是哪件出了问题。以后可以做成自动二分，见阶段 4。
- **移出某件 / 交回某件给 Bot**：被交回的事项回到 `run`，Bot 收到失败日志的摘要。
- **放弃这批**：条目回到下一批的 `queued`，暂停这条通道的自动触发，等你处理。

**重试原则**：构建失败、健康检查失败这类问题，重跑大概率还是一样的结果，所以**不自动重试**。“另一个发布正在进行”、Hub 正在 drain 这类情况本来就不算失败：调度器发现通道忙，就让批次停在 `awaiting` 或 `collecting` 等待，空闲后再执行。直通发布和批次发布共用现在的 `st.releasing` 锁，同一时刻只跑一个。

**PR 批次**：逐个执行。单个 PR 推送或合并失败（冲突、检查变红、合并状态变成 dirty）时，只把这个条目标记为失败或留到下一批，其余照常进行，批次记为 `partial`。前一个 PR 合并后，后面的 PR 可能落后于 base。合并前要重新读一次 `mergeable`，不是 `clean` 的留到下一批，等 sync 再次判定就绪后自动重新入队。

### 4.6 重启与恢复

批次状态在库里，自动触发靠 `due_at` 判断，所以重启不丢任何东西：

- 启动时，`running` 的批次按 `last.json` 的 `batch` 字段对账（现有的 `report_release` 扩展一下即可）。对不上而且脚本已经不在了，就按“没有报告结果”处理。
- `sealing` 的批次（集成到一半 Hub 被重启了）回到 `collecting` 重新冻结。集成是幂等的：rebase 已经做过的分支，再做一次是空操作。
- `awaiting` 的审批卡本来就持久化在 decisions 表里，不需要额外处理。

## 5. 触发方式

调度器每 30 秒巡检一次，和 `watch_releases` 一样是常驻任务，同时在入队和批次结束时主动检查一次。`collecting` 批次满足**任意一个**条件就触发：

| 条件 | 含义 | 适合 |
| --- | --- | --- |
| 手动 | 交付页或事项页点“立即交付”；在对话里说“把队列发了” | 随时 |
| 静默期 `quiet_s` | 最后一次入队后 N 分钟内没有新条目 | “短时间内针对同一项目的一串改动”，攒完再发 |
| 数量 `max_count` | 条目数达到 N | 防止一批过大，出问题不好定位 |
| 等待时长 `max_age_s` | 最早的条目已经等了 T | 防止持续有新条目、静默期一直不满足而永远发不出去 |
| 定时窗口 `windows` | 到了设定的时刻（例如 12:00、18:00） | 希望固定时段才重启的场景 |

另有两个约束：

- `min_gap_s`：离上一次执行结束不足这个间隔时，先不执行，`due_at` 顺延。这是“发布不要太频繁”的硬保证。
- 通道忙（有发布在跑、Hub 在 drain）或 `paused` 时不触发。

**建议给 Steve 项目的默认值**（待确认，见 §11 问题 3）：`release` 通道用 `queue`，静默期 20 分钟，最多等 2 小时，满 5 件就发，两次之间至少隔 30 分钟，不设定时窗口，每批审批一次。

## 6. 与卡片、事项状态的关系

| 阶段 | 事项 `status` | `stage` / `now` | 卡片（待处理） |
| --- | --- | --- | --- |
| 验收通过、已入队 | `ship`（新状态“待交付”，见 §11 问题 4） | 收尾 / “已入队 · DQ-3，预计 14:20 交付” | 无 |
| 集成时被退回 | `run` | 收尾 / “rebase 有冲突，已交回” | 无（消息发给 Bot） |
| 批次等批准 | `ship` | 收尾 / “DQ-3 等你批准” | **一张**批次审批卡，列出所有条目 |
| 交付中 | `ship` | 收尾 / “正在发布（DQ-3）” | 无 |
| 已交付 | `done` | 收尾 / “已发布 <版本>（DQ-3）” | 无；通知栏一条“DQ-3 已发布 3 件” |
| 批次失败 | `ship`（条目回队列）或 `run`（被交回） | “DQ-3 发布失败，等你决定” | **一张**批次异常卡 |

要点：

- **入队不等于完成**。延续“彻底完成才算完成”的原则 [K1]，只有交付成功的事项才会进入 `done`。
- **新增 `ship`“待交付”状态**：不算“进行中”（Bot 已经没事可做），也不算“等你”（只有批次卡才需要你处理），但仍然是未完成。对应改动：`ItemExt::is_settled`（`steve-core/src/repo/mod.rs`）加入 `ship`，像 `pause` 一样不被 `settle()` 改写；`is_finished` 不加，因此不会触发 `tidy_branch` 或依赖它的后续事项；前端 `ST` 映射加一项（中性色）；事项列表的筛选加“待交付”。
- **用户追加新消息**：和现在一样回到 `run` [K1]，同时把条目撤回（`withdrawn`）。Bot 再次交付、验收通过后会重新入队。
- 审批卡和异常卡的 `subject_type` 是 `delivery_batch`，`item_id` 留空（批次不属于某一件事项）。卡上列出每件事项，可以点进去。每件事项的时间线里各写一条“已进入 DQ-3”“DQ-3 已发布”。

## 7. Hub 接口

REST，挂在 `/api` 下，和现有路由一样经过 `Caller` 层：

| 路由 | 作用 |
| --- | --- |
| `GET /api/delivery?project=&kind=&status=` | 列出批次（附带条目），交付页使用 |
| `GET /api/delivery/batches/:id` | 批次详情：条目、触发原因、审批卡、日志路径、结果 |
| `POST /api/delivery/batches/:id/flush` | 立即交付（手动触发） |
| `POST /api/delivery/batches/:id/retry` | 重试整批（失败或 partial） |
| `POST /api/delivery/batches/:id/split` | 拆成逐件的小批次 |
| `POST /api/delivery/batches/:id/cancel` | 放弃这批，条目回到下一批 |
| `POST /api/items/:id/enqueue` `{kind?}` | 把事项加入队列（kind 缺省时按项目推断） |
| `POST /api/items/:id/deliver-now` `{kind?}` | 跳过队列，走现在的直通流程（紧急通道） |
| `DELETE /api/delivery/entries/:id` | 移出队列 |
| `PATCH /api/delivery/entries/:id` `{pos?, batch_id?}` | 调整顺序或移到下一批 |
| `GET /api/projects/:id/delivery-policy` | 读通道策略 |
| `PUT /api/projects/:id/delivery-policy/:kind` | 改通道策略 |

其他接入点：

- **审批**：`POST /api/decisions/:id` 已经是通用接口，只需在 `orchestrator.rs` 的决策分发里加 `("approve", "delivery_batch")` 和 `("fail", "delivery_batch")` 两个分支。
- **SSE**：加 `ChangeEvent::Delivery { id, ws }`；`GET /api/state` 加一个轻量的 `delivery` 摘要（每个项目收集中几件、有没有在跑），给侧栏角标用。
- **MCP**：
  - Bot 侧：`steve_publish` 在 `queue` 通道下改为入队并返回批次号；工具说明里补一句“项目开了交付队列时，这一步只是入队”。
  - 主 Agent 侧：加 `steve_delivery_status`（只读，可以加入自动允许名单）和 `steve_delivery_flush`。后者只是触发，审批照常进行。
- **配置对话**：可以通过现有的 changes/draft 机制注册 `delivery_policy` 目标，让用户在对话里说“Steve 项目改成每天 12 点和 18 点发”就生成一份可确认的修改。这一项放在阶段 2，可选。
- **代码位置**：新建 `hub-rs/crates/steve-core/src/delivery.rs`（批次状态机、集成、调度），仓储放在 `steve-store/src/repo/delivery.rs`，路由加在 steve-hub，同时更新 `ROUTES.md`。PR 部分在 `steve-github/src/accept.rs` 里改为调用入队接口，而不是开卡。

## 8. 对现有发布脚本的改动范围

改动小，而且向后兼容：

1. **多事项参数**：`release.sh [--web|--hub] <branch> [item-id...]`。`ITEM` 改为数组 `ITEMS`，第一个仍然写进 `item` 字段，保证老的 Hub（以及回滚后的 TS Hub）读 `last.json` 不出错。
2. **`last.json` / `web/last.json` / `history.jsonl`**：新增 `"items":[...]` 和 `"batch":"$STEVE_BATCH"`（可以为空）。
3. **日志**：在头部打印 `batch=… items=…`，方便从批次页跳到日志。
4. **不改的部分**：快进合并、按改动判断 hub/web、drain、备份、健康检查、回滚、发布锁，全部照旧。集成分支由 Hub 准备好，脚本看到的仍然只是一个分支。
5. **测试**：`ops/test-release.sh` 加两个用例：“多事项、成功”和“多事项、健康检查失败后回滚”，检查 `last.json` 里的 `items` 和 `batch`。

Hub 侧对应的改动：`report_release` 读 `items`，对每件事项分别 `finish` 或 `release_failed`；有 `batch` 时更新批次。`start_release` 拆出一个只负责运行脚本的公共部分，供直通和批次两种调用方共用；审批结果（`release_outcome`）在批次模式下写到批次的审批卡上。

## 9. Web 界面

**新页面「交付」**（`#/delivery`，侧栏放在“项目”下面，有收集中的条目时显示数量角标）：

- 按项目分组，每个项目显示它的通道（发布、开 PR、合并 PR）。
- **收集中的批次卡**：
  - 标题“DQ-7 · 收集中 · 3 件”，右侧显示触发提示，例如“静默 12 分钟后自动交付”或“18:00 窗口”。倒计时根据 `due_at` 计算。
  - 条目列表：事项编号和标题（可点击）、分支、领先 main 的提交数、影响范围标记（Hub / 前端）、入队时间。可以拖动排序，每行有“移出”。
  - 按钮：**立即交付**（主按钮）、暂停自动触发。
- **进行中的批次卡**：步骤条“集成 → 等批准 → 构建 → 等回合结束 → 重启 → 健康检查”，当前步骤高亮；被退回的条目用删除线表示，并附原因。
- **历史**：已结束的批次，显示结果（版本号、成功、部分成功或失败）、耗时，以及“查看日志”“重试”。

**其他页面上的变化**：

- **事项详情**：顶部显示交付标记，例如“已入队 · DQ-7”“交付中”“已发布 2026…（DQ-7）”；收尾阶段提供“加入交付队列”“立即发布（跳过队列）”“移出队列”。
- **事项列表**：新增“待交付”状态的点和筛选。
- **待处理**：批次审批卡标题为“发布 · DQ-7（Steve，3 件）”，正文列出每件事项和它的交付摘要，并说明是否会重启；批次异常卡提供 §4.5 的各个选项。
- **项目设置**：在原来的“发布”一行下新增“交付策略”：直通或队列，以及静默期、数量、等待时长、定时窗口、最小间隔、是否每批审批。
- **对话右栏通知**：每个批次只发一条，例如“DQ-7 已发布 3 件 · 2026…”或“DQ-7 发布失败”。失败类通知按现有规则同时在主对话流里提醒。

界面沿用 `web/DESIGN.md` v6 的卡片、列表和状态色，不引入新组件。步骤条可以复用现有的 trace 组件。

## 10. 分阶段实施

| 阶段 | 内容 | 验证 |
| --- | --- | --- |
| **0 · 脚本准备** | `release.sh` 支持多事项和 `STEVE_BATCH`；`report_release` 支持 `items`；补测试。行为不变，可以单独发布。 | `ops/test-release.sh`、steve-core `release_outcome` 测试 |
| **1 · 发布队列（手动触发）** | 013 迁移和仓储；`delivery.rs` 状态机；验收后入队；`steve_publish` 改为入队；集成分支；批次审批卡和异常卡；执行、结果与重启恢复；`ship` 状态；REST、SSE；Web 交付页（收集中、进行中、历史）、事项标记、待处理卡。策略默认都是 `direct`，只有用户打开后 Steve 项目才进入队列模式。 | 单元测试：合并同类项、退回、失败回队列、重启恢复；e2e：两件事项入队后一次发布、一次重启；界面截图 |
| **2 · 自动触发** | 调度器：静默期、数量、等待时长、定时窗口、最小间隔；`due_at` 倒计时；项目设置里的策略编辑；主 Agent 的 `steve_delivery_status` / `flush`；对话里改策略（可选）。 | 用可注入时钟测试各种触发条件 |
| **3 · PR 队列** | `pr_open`、`pr_merge` 通道：`offer_pr` 和 `ask_merges` 改为入队；批次逐个推送、开 PR 或合并；合并前重新检查可合并状态；部分失败的处理。 | steve-github 用 fake GitHub 测试多个 PR 的批量开和批量合并 |
| **4 · 打磨** | 失败后自动二分；可选的自动审批（只对低风险通道，例如只改前端的批次）；集成后、发布前先跑一次类型检查和测试再开审批卡，提前拦住坏批次；交付统计（每周发布次数、平均每批件数）。 | 视需要 |

阶段 0 和 1 做完就能解决“一卡一发”的主要痛点。阶段 1 的工作量估计在 1.5k–2k 行左右（Rust 加前端，含测试）。

## 11. 需要你确认的问题

1. **批次还要不要审批？** 建议每批审批一次。待处理从“每件一张卡”变成“每批一张卡”，对外生效前仍然有人确认。自动审批放到阶段 4，作为可选项。
2. **入队要不要还经过 Bot？** 建议验收通过后由 Hub 直接入队，不再让 Bot 去调 `steve_publish`。这样少一个回合，也避免 Bot 忘了调用。
3. **默认触发值**：静默 20 分钟、最多等 2 小时、满 5 件、两次至少隔 30 分钟、不设窗口。可以吗？还是你更想要固定窗口（比如 12:00 和 18:00）？
4. **事项状态**：建议新增“待交付”（`ship`）状态。另一种做法是保持“进行中”，只在 `now` 里写“已入队”，改动更小，但列表里看不出哪些事项只是在等发布。
5. **失败后的默认动作**：建议整批回到队列、开一张异常卡、不自动重试。要不要一开始就做自动二分？
6. **紧急通道**：是否保留单件“立即发布（跳过队列）”？建议保留。
7. **PR 怎么批**：多件事项是“各开各的 PR、一次审批”（建议），还是“合成一个 PR”？后者 review 起来更省事，但事项和 PR 不再是一一对应。
8. **范围**：策略按项目配置（建议），还是工作空间统一配置？除了 Steve 以外，其他配置了发布脚本的项目是否也默认用队列？
9. **只改前端的批次**：前端发布不重启 Hub，代价小。要不要给它单独一条更快的通道（比如静默 5 分钟），而 Hub 通道攒得更久？建议先不拆，看实际频率再定。
