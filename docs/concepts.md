# Core Concepts

This guide explains the domain model behind Steve: how conversations become tracked work, how decisions are consolidated, how harnesses are governed without rewriting their settings, and where boundaries sit.

## Steve as coordinator

In a Steve workspace, **Steve** (the main bot) is the **coordinator**, not an implicit worker.

- When a message becomes a tracked item, Steve dispatches it to a configured worker **Bot** (or a workspace **Team** of Bots bound to specific execution Nodes). Steve itself is never silently picked as an automatic item executor.
- If no eligible worker Bot can take the work (none enabled, or a Team has no reachable Node/Bot), Steve still creates the item unassigned and opens a single deduplicated **No executor** (`no_executor`) decision card with rejection reasons and an optional Bot proposal. A workspace's `noExecutor` policy (`Ask` by default, or `Steve`) controls whether Steve asks first or accepts deferred execution.
- In the **instance owner's private main conversation**, asking Steve to inspect, organize, or fix something on the Hub host lets inline execution run with that OS user's permissions while `steve-confine` keeps Steve's own keys, database, and binaries out of reach. For all other contexts, inline research is read-only across three layers (adapter `plan` mode, ACP kind/path guard, OS read-only sandbox).

## Chat routing and durable threads

Every message in the main conversation passes through a per-space serial durable reply pipeline (`thread_reply_tasks` with lease fencing, idempotency keys, and immutable `thread_reply_receipts`):

- **Deterministic shortcuts first**: Explicit commands (`/汇报`), `@Bot` mentions, quoted item focus, short confirmations of an open offer, onboarding, and obvious greetings bypass the router model completely.
- **Nine routes**: When classification is needed, the router proposes one of `answer`, `research`, `item`, `suggest`, `ask`, `supplement`, `status`, `config`, or `reroute`. Code-level guards can overrule the model (for example, preventing a closed item from being silently supplemented, or preferring read-only research plus an offer over prematurely creating an item).
- **Conversational configuration**: Requests like *"Use Opus for the backend bot"* draft field-level changes through `steve-modules` transactional SQLite changesets with conflict checks (`409`) and 24-hour undo across restarts.
- **Interactive surfaces (A2UI)**: Replies may include `a2ui` blocks (forms, choices, tabs, charts) rendered declaratively by the client with no arbitrary code execution and single-submission semantics across tabs and reloads.

## Items and delivery semantics

An **Item** is the unit of tracked work.

- **Lifecycle**: `ready → active → closing → closed`, plus `paused`. Reopening a closed item starts a new cycle (`cycle + 1`) while preserving prior cycles, plans, and deliveries.
- **Five-gate closing**: Moving through `closing` freezes the git worktree's content-addressed result and checks five gates before closing: (1) no active turns or queued human messages, (2) no open required operations (such as an in-flight release), (3) the frozen result has not drifted or is verified on the default branch, (4) acceptance is settled (`automatic` by default, `human`, or `waived`), and (5) no open question or approval was raised after close began.
- **Type and Delivery tags**: Each item carries one **Type** tag (`Fix`, `Feature`, `Docs`, `Refactor`, `Ops`, `Chore`) and multi-select **Delivery** tags (`Plan`, `Code`, `Report`, `Content`, `Data`, `Operation`) so outcomes are distinguished from work categories.
- **Design gate**: When a goal asks for a design without authorizing implementation, the delivered design pauses for an explicit decision (`implement`, `keep design only`, or `revise`). Workspace permission levels cannot bypass this gate.
- **Goal edits**: Editing an item's goal sends the updated version (`vN`) directly to the running session without replacing the session behind the agent's back.
- **Worktrees, outputs, and preview links**: Git items run in isolated worktrees on `steve/<id>` branches with disk-floor protection. Non-Git deliverables persist in `<STEVE_HOME>/workspaces/<item>/outputs`. Bots (`steve_deliver_file`) and humans (`steve publish <file|folder>`) can publish immutable Hub-hosted preview links (static HTML by default, or an isolated sandboxed interactive frame with no access to Steve or the network) and optional revocable share tokens.
- **Pull request merging**: For git projects with a *merged* (or *merged and tagged*) completion rule, agents commit and open a pull request on the host but never push or merge the default branch. Steve never enables host auto-merge; after acceptance and a human merge approval bound to the exact repository, PR number, full head SHA, and item cycle, Steve merges once CI passes via `gh pr merge --match-head-commit`.
- **Dependencies and forks**: Prerequisite graphs are cycle-checked and governed by durable `WaitContract` timeouts. Any point in a session transcript can be forked into a new session or child item and inspected in the relation graph.

## Decisions (single entry)

Every human action — reviews, approvals, agent questions, proposals, failures, and confirmations — lands in one place: **Decisions** (`#/inbox`, Chinese: **待我决定**).

- **Six kinds**: `accept` (Review), `approve` (Approval), `ask` (Question), `propose` (Proposal), `fail` (Problem), and `confirm` (Confirm).
- **Three tiers**:
  - `decide`: Actionable cards counted in the sidebar badge (delivery reviews, SHA-pinned PR merges, ACP tool permissions, agent questions, `no_executor` coordinator cards, `handoff_unavailable`, vault `secret_use`, design gates, mail send approvals, project `AGENTS.md` proposals, flow approvals).
  - `draft`: Grouped outbound channel reply drafts (no badge noise).
  - `folded`: Collapsed one-line entries (already replied, expired, or unsendable) expandable in place.
- Decisions support **Snooze** and a **Handled** tab covering the last 7 days with 24-hour undo where reversible.

## Permissions

Steve does not simulate automatic approvals or rewrite your harness configuration files. Each workspace has a three-level **Permissions** setting (`ApprovalMode`: `ask`, `auto`, `full_access`), controlled only by the workspace owner and defaulting to **Ask**.

- **Ask (`ask`)**: Sessions use the harness's asking mode. Permission requests that reach Steve open a decision card for the owner.
- **Auto (`auto`)**: Requests `PermissionMode::Auto` only on harnesses that declare native automatic approval (`native_approval`). Unsupported or refusing harnesses are shown honestly as unsupported or refused — never simulated.
- **Full access (`full_access`)**: Requests the harness's declared full-access mode (`modes.full_access`, `mode_args.full_access`, or `mode_meta.full_access`). Switching to **Full access** requires confirming a risk dialog in the UI (and `confirmed: true` in the API) stating that the harness stops asking before commands, can modify/delete writable files and use the network, Steve's sandbox still protects Steve's own files, and self-sandboxed harnesses (`nested_sandbox = false`) running outside Steve's sandbox can still reach those files.

| Harness | Auto | Full access |
| --- | --- | --- |
| Claude Code | `auto` | `bypassPermissions` |
| Codex | `agent` (inside Steve's sandbox its inner Seatbelt cannot start; `auto_review` auto-approves safe escalated commands and denies/escalates risky ones) | `agent-full-access` (skips inner sandbox, 0 prompts; still inside Steve's sandbox) |
| Antigravity CLI | `auto` (inside Steve's sandbox its own terminal sandbox cannot start; its classifier lets safe commands run and asks before risky ones) | `--dangerously-skip-permissions` (still inside Steve's sandbox) |
| goose | `smart_approve` | `auto` |
| Mistral Vibe | `smart-approve` | `auto-approve` |
| Pi | Unsupported | Native (`full_access_only`, requires Bot configured for `full_access`) |
| Other built-ins declaring full access (Gemini CLI, GitHub Copilot CLI, Qwen Code, Kimi Code, Grok Build, Factory Droid, CodeBuddy Code, Qoder CLI) | Unsupported | Declared `modes.full_access` / `mode_meta.full_access` |
| OpenCode, Kilo Code, Cursor, Kiro CLI | Unsupported | Unsupported |

- **Invariants**:
  - `plan` and read-only sessions are never widened.
  - A Node owner's ceiling (`PolicyCeiling`) still caps the widest mode allowed on that Node.
  - Running turns keep their current mode; changes take effect at the next idle boundary or session open.
  - Agent questions, Steve tool permissions, vault secret use, personal external sends, design decisions, and SHA-pinned PR merges always require human confirmation.

## Deterministic session handoff

When a session hits a provider quota limit, repeated provider errors, or an explicit owner handoff request (`HandoffReason::QuotaExhausted | ProviderError | OwnerRequest`), Steve performs a deterministic working-memory handoff:

- **Same Bot, fallback surface**: By default the item stays owned by the same Bot (preserving its identity, permissions, memories, and Team) and switches only its execution surface (`harness + model + effort`) from the Bot's configured fallback list.
- **Quota wait vs switch**: With `quotaAutoHandoff` enabled (default), Steve switches automatically unless the provider's quota reset is within `quotaWaitMinutes` (default `10`), in which case it waits for reset. If every fallback surface on that Bot is unavailable, Steve opens a `handoff_unavailable` coordinator card rather than switching Bots silently.
- **Offline Node handover**: If a remote Node goes offline with messages waiting for a session for at least `STEVE_NODE_HANDOFF_MIN` minutes (default `30`; `0` disables), Steve closes that session and continues the item on another eligible Node.

## Subagents and background tasks

Steve tracks subagents, background tasks, and harness-native workflows through structured `subtask` events in the `steve.v1` transcript rather than matching hardcoded tool names in the UI:

- Child tool calls nest under their parent subtask and deduplicate launcher rows.
- Verified runtime adapters (`Claude Code`, `Codex`, `Antigravity CLI`, `Kimi Code`, `Pi`) surface live status, step counts, elapsed time, and adapter-reported token/cost figures without fabricating unreported fields.
- Active subtasks appear in the session header's running capsule and expose per-subtask **Cancel** whenever the adapter advertises `can_cancel`.

## Identity, avatars, and navigation

- **Avatar as identity**: Every actor resolves to one of five kinds (`person`, `bot`, `system`, `org`, `team`) rendered through a single `ActorAvatar` component. People show circular photos or deterministic initials; Bots show distinct customizable robot avatars; harnesses and model makers are shown separately via `HarnessMark` and `ModelMark` inside `SessionIdentity` (`Bot · Harness · Model · Node · Status`).
- **Menu information architecture**: The desktop sidebar organizes workspaces into five scannable groups — Primary (`Chat`, `Decisions`, `Items`, `Projects`), Schedule (`Calendar` with next meeting time, `My todos` with due count), Messages & people (`Feed`, `Chats`, `Mail`, `Contacts`), Library (`Knowledge`, `Connections`), and Bots & automation (`Bots`, `Conversations`, `Automation`, `Capabilities`, `Signals`) — with **Stats** pinned in the footer alongside the **Account** menu (`Me`, `Devices`, `Accounts`, `Settings`, `Sign out`) and theme toggle. On phones, a five-tab bottom bar (`Chat`, `Decisions`, `Items`, `Calendar`, `My todos`) pairs with a top-left **Me** hub.

## Knowledge, digest, and people

- **Redefined knowledge boundary**: Personal knowledge stores **people, organizations, projects, owner preferences/rules, commitments, and owner-provided materials** (notes, files, memories, read-only folders such as a wiki or Obsidian vault). Work items and chat history are intentionally excluded from knowledge indexing and searched separately (`scope=records|all`, default `knowledge`).
- **Project `AGENTS.md` proposals**: Codebase engineering lessons discovered during work route to project proposals (`agents_proposal` confirmation cards on Project pages) rather than polluting personal memory; confirming records acceptance for a later PR without directly editing repositories.
- **Gated steward & disclosure levels**: Scheduled knowledge extraction is deterministically gated (`knowledge.steward.threshold`) so idle periods do not trigger model calls. Every entry carries a disclosure level (`public`, `conclusion`, `reasoning`, `never`). Secrets and `never` entries are never indexed. Outbound messages to external audiences pass both deterministic rules and a cross-model audit (`exclude_model` avoids the drafting model).
- **Conversation digest**: `steve-digest` deterministically windows and extracts facts from already-stored channel messages and mail threads for an opt-in ordinary Digest Bot — no automatic history/attachment download and no model calls inside the digest crate.
- **Contacts and teams**: Contacts, organizations, virtual or Feishu-synced teams (usable as visibility scopes), holdings, stewards, and reversible merges.

## Connections, automation, feed, and stats

- **Connections**: Built-in **Feishu / Lark** (QR bot + OAuth self, spaces, personas, thread-to-item routing), **Email** (IMAP/SMTP with IDLE push, sandboxed HTML, server-computed recipients, content-bound send approval + disclosure audit), **GitHub** (GitHub App or fine-grained PAT, HMAC webhooks + poll fallback, PR/issue linking, collaborator review routing, CI failure flowback), **Calendar** (`steve-calendar`: read-only ICS subscription/upload and `calendar.v1` connector plugins, meeting deduplication, 4 read-only MCP tools), plus external `ext_ws` / `ext_ws.bot` plugins.
- **Automation (`steve-flow`)**: Durable step-committed graphs with 11 built-in node kinds (`trigger`, `filter`, `table`, `flow`, `wait`, `approve`, `action`, `script` in QuickJS, shell-quoted `cmd`, SSRF-guarded `http`, `bot`), versioned edits, dry runs, per-run cost budgets, natural-language/cron schedules, and deduplicated signals with one-click undoable corrections.
- **Feed & notifications**: Scheduled workspace reports (shared visibility, per-person read state) and opted-in private personal notifications (`work.item.done`, `briefing.ready`) with IANA timezone-aware quiet hours.
- **Stats & durable usage ledger**: Unified accounting across sessions, items, and workspace stats (including persisted one-shot Think/Research bills, Models.dev daily price sync, cache hit rate/savings, lead/cycle/wait time, first-pass rate, and rework) plus a `/api/stats/realtime` 10-second output TPS trend API.
