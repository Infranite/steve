# hub-rs

Rust rewrite of the Steve Hub. Crates (each owned by one worker until integration):

| crate | replaces (TS) | scope |
|---|---|---|
| steve-store | db.ts, repo.ts | SQLite (identical schema and migrations), typed repositories, change bus hook |
| steve-acp | acp.ts, codex.ts | ACP client over stdio to adapter processes |
| steve-mcp | mcp.ts | Steve MCP tools over streamable HTTP, bearer per session |
| steve-auth | auth.ts | OIDC login/callback/logout, cookie sessions, dev login |
| steve-core | orchestrator.ts, lifecycle.ts, thread.ts, gateway.ts, changes.ts | item lifecycle, routing, conversational config |
| steve-hub | main.ts, api.ts, bus.ts, config.ts, system.ts, housekeeping.ts | binary: axum server, REST + SSE, static web, config, background jobs |
| steve-modules | – | shared base of the new modules: API errors, extractors, changesets, decision dispatch, registries |
| steve-people | – | contacts, organizations, teams (modules/contacts.md) |
| steve-vault | – | accounts, connections, secret vault (modules/accounts-vault.md) |
| steve-flow | – | signals, automation, schedules (modules/signals-automation.md, schedules.md) |
| steve-channels | – | channels, personas, Feishu (modules/channels-personas.md, feishu.md) |
| steve-knowledge | – | knowledge base (modules/knowledge.md) |
| steve-mail | – | email (modules/email.md) |
| steve-github | – | GitHub (modules/github.md) |
| steve-research | – | inline read-only research runs for the main thread (modules/thread-routing.md §4, R2) |

Contract: `~/Agents/steve-rs-plan/api-inventory.md`; plan: `~/Agents/steve-rs-plan/rust-hub-plan.md`.
Never point a Rust Hub at `~/.steve` or port 8440 until cutover.
