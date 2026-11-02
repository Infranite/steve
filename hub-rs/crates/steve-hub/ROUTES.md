# steve-hub route checklist

Every route of the TS Hub (api.ts, auth.ts, main.ts at TS main 1d16430 + attachments) and its state in the Rust Hub.
Contract = steps identical in `tests/contract/contract.mjs --a ts --b rust` (last run: 259/259). e2e = exercised by `tests/e2e.sh` mainline/restart/attach (all pass on the Rust binary).

| route | Rust | contract | e2e | notes |
| --- | --- | --- | --- | --- |
| `GET /healthz` | done | ✓ 1/1 |  |  |
| `GET /auth/login` | done | ✓ 1/1 |  |  |
| `GET /auth/callback` | done | ✓ 1/1 |  | full OIDC flow covered by steve-auth tests/login.rs, not by the contract |
| `POST /auth/logout` | done | ✓ 2/2 |  |  |
| `GET /auth/dev` | done | ✓ 1/1 | ✓ |  |
| `POST /internal/release/drain` | done | ✓ 1/1 |  |  |
| `GET /internal/release/drain` | done | ✓ 1/1 |  |  |
| `GET /api/me` | done | ✓ 6/6 |  |  |
| `GET /api/state` | done | ✓ 8/8 | ✓ |  |
| `POST /api/workspaces` | done | ✓ 7/7 | ✓ |  |
| `PATCH /api/workspaces/:id` | done | ✓ 3/3 |  |  |
| `POST /api/workspaces/:id/switch` | done | ✓ 2/2 |  |  |
| `POST /api/thread` | done | ✓ 7/7 | ✓ |  |
| `POST /api/thread/stop` | done | ✓ 1/1 |  |  |
| `POST /api/notifications/read` | done | ✓ 5/5 |  |  |
| `POST /api/thread/:id/promote` | done | ✓ 3/3 |  |  |
| `POST /api/thread/:id/reroute` | done (thread-routing R3) | `thread_routing_api` |  |  |
| `POST /api/thread/:id/offer` | done (thread-routing R3) | `thread_routing_api` |  |  |
| `POST /api/thread/:id/suggest` | done (thread-routing R3) | `thread_routing_api` |  |  |
| `POST /api/thread/runs/:id/stop`, `DELETE /api/thread/runs/:id` | done (thread-routing R3) | `thread_routing_api` |  |  |
| `PATCH /api/items/:id/thread` | done (thread-routing R3) | `thread_routing_api` |  |  |
| `POST /api/items` | done | ✓ 7/7 | ✓ |  |
| `POST /api/items/bulk` | done | ✓ 3/3 |  |  |
| `GET /api/items/:id` | done | ✓ 16/16 | ✓ |  |
| `POST /api/items/:id/actions` | done | ✓ 6/6 | ✓ |  |
| `POST /api/items/:id/goal` | done | ✓ 2/2 |  |  |
| `PATCH /api/items/:id` | done | ✓ 5/5 |  |  |
| `DELETE /api/items/:id` | done | ✓ 2/2 |  |  |
| `POST /api/items/:id/supplement` | done | ✓ 3/3 | ✓ |  |
| `POST /api/items/:id/deliver` | done | ✓ 1/1 |  |  |
| `POST /api/items/:id/reassign` | done | ✓ 2/2 |  |  |
| `POST /api/items/:id/deps` | done | ✓ 3/3 |  |  |
| `DELETE /api/items/:id/deps/:dep` | done | ✓ 1/1 |  |  |
| `POST /api/items/:id/snooze` | done | ✓ 3/3 |  |  |
| `GET /api/sessions` | done | ✓ 2/2 |  |  |
| `GET /api/sessions/search` | done | ✓ 3/3 |  |  |
| `GET /api/sessions/:id` | done | ✓ 5/5 | ✓ |  |
| `GET /api/sessions/:id/events` | done | ✓ 6/6 | ✓ |  |
| `POST /api/sessions/:id/messages` | done | ✓ 3/3 |  |  |
| `POST /api/sessions/:id/cancel` | done | ✓ 1/1 |  |  |
| `POST /api/sessions/:id/end` | done | ✓ 1/1 |  |  |
| `PATCH /api/sessions/:id` | done | ✓ 3/3 |  |  |
| `DELETE /api/sessions/:id` | done | ✓ 2/2 |  |  |
| `POST /api/sessions/:id/fork` | done | ✓ 2/2 |  |  |
| `POST /api/sessions/:id/link` | done | ✓ 2/2 |  |  |
| `DELETE /api/queued/:id` | done | ✓ 1/1 |  |  |
| `GET /api/local-sessions` | done | ✓ 2/2 |  |  |
| `POST /api/local-sessions/import` | done | ✓ 3/3 |  |  |
| `POST /api/decisions/:id` | done | ✓ 5/5 | ✓ |  |
| `POST /api/decisions/:id/dismiss` | done | ✓ 2/2 |  |  |
| `POST /api/decisions/:id/undo` | done | ✓ 2/2 | ✓ |  |
| `GET /api/bots/:id/prompt` | done | ✓ 4/4 |  |  |
| `POST /api/bots` | done | ✓ 3/3 | ✓ |  |
| `POST /api/bots/draft` | done | ✓ 2/2 |  |  |
| `PATCH /api/bots/:id` | done | ✓ 3/3 | ✓ |  |
| `PUT /api/bots/:id/workspaces` | done | ✓ 3/3 |  |  |
| `POST /api/bots/:id/duplicate` | done | ✓ 1/1 |  |  |
| `DELETE /api/bots/:id` | done | ✓ 5/5 |  |  |
| `POST /api/bots/:id/restore` | done | ✓ 2/2 |  |  |
| `GET /api/bots/:id/memories` | done | ✓ 1/1 |  |  |
| `POST /api/bots/:id/memories` | done | ✓ 3/3 |  |  |
| `PATCH /api/memories/:id` | done | ✓ 2/2 |  |  |
| `DELETE /api/memories/:id` | done | ✓ 1/1 |  |  |
| `DELETE /api/memories` | done | ✓ 1/1 |  |  |
| `POST /api/projects` | done | ✓ 8/8 | ✓ |  |
| `POST /api/projects/describe` | done | ✓ 3/3 |  |  |
| `PATCH /api/projects/:id` | done | ✓ 3/3 |  |  |
| `GET /api/fs/dirs` | done | ✓ 4/4 |  |  |
| `POST /api/tags` | done | ✓ 5/5 |  |  |
| `PATCH /api/tags/:id` | done | ✓ 4/4 |  |  |
| `DELETE /api/tags/:id` | done | ✓ 2/2 |  |  |
| `POST /api/tags/:id/merge` | done | ✓ 2/2 |  |  |
| `GET /api/settings/schema` | done | ✓ 1/1 |  |  |
| `PATCH /api/settings` | done | ✓ 5/5 |  |  |
| `POST /api/config/draft` | done | ✓ 3/3 |  |  |
| `POST /api/config/:id/apply` | done | ✓ 3/3 |  |  |
| `POST /api/config/:id/undo` | done | ✓ 2/2 |  |  |
| `POST /api/files` | done | ✓ 10/10 | ✓ |  |
| `GET /api/files/:id` | done | ✓ 5/5 | ✓ |  |
| `GET /api/devices` | done | ✓ 2/2 |  |  |
| `PATCH /api/devices/:id` | done | ✓ 3/3 |  |  |
| `GET /api/stream` | done | ✓ 3/3 |  | event sequences compared (3 windows incl. a second-workspace subscriber) |
| `POST /mcp` | done | ✓ 2/2 | ✓ | tools exercised end to end by e2e (update/ask/deliver) |
| `GET /release.json` | done | ✓ 1/1 |  | never served (the web release stamp) |
| `GET /*` | done | ✓ 5/5 |  | web dir read per request, caching, missing `/assets/*` 404, 503 without web: also `web_dir_is_read_per_request` |

## Rust-only routes (not in the TS Hub; covered by `tests/modules.rs`, not by the contract)

| routes | from | test |
| --- | --- | --- |
| `GET /api/items/:id/panels`, `POST /api/items/:id/panels/:panel/actions/:action` | steve-modules B1 panels | `panels_and_mentions` |
| `/api/contacts…`, `/api/me/contact`, `/api/parties/…`, `/api/attrs…`, `/api/attr-defs…`, `/api/orgs…`, `/api/teams…` | steve-modules contacts (+ merge/changes registered) | `modules_are_mounted_behind_the_login`, `module_events_stay_in_their_workspace` |
| `/api/systems`, `/api/accounts…`, `/api/match-rules…` | steve-modules accounts | `modules_are_mounted_behind_the_login` |
| `/api/vault/status|init|recover|rotate`, `/api/vaults`, `/api/vault/templates`, `/api/vault/items…` | steve-modules vault V1/V2, reveal behind V3 passkeys | `modules_are_mounted_behind_the_login` |
| `/api/schedules…` | steve-flow S7 | `modules_are_mounted_behind_the_login` |
| `/api/kb/…` | steve-knowledge K1–K4 | `knowledge_is_mounted_and_scoped`, `knowledge_note_is_found_over_http_and_mcp` |
| `/api/passkeys…` | steve-modules vault V3 | `connections_passkeys_and_github_are_mounted` |
| `/api/connections…`, `GET /auth/connect/callback` (session optional) | steve-modules accounts A2 | `connections_passkeys_and_github_are_mounted` |
| `POST /api/github/app/manifest`, `GET /api/github/app/manifest/callback`, `GET /api/github/app/installed` (GETs session optional) | steve-github G1 | `connections_passkeys_and_github_are_mounted` |
| `/api/signals…`, `/api/sources…`, `/api/flows…`, `/api/runs…` | steve-flow S1/S2 (GraphEngine) | `wired::signal_modules_mount_their_routes` |
| `/api/github…` (repos, PRs, issues, deliveries) | steve-github G2 | `wired::github_signals_reach_the_signals_page` |
| `POST /hooks/github`, `POST /hooks/:connection_id` (no session; 401 empty on a bad signature, 413, 429) | steve-github G2, steve-flow S1 | `wired::channel_messages_reach_the_signals_page_and_hooks_verify` |
| `GET/PATCH /api/mail/accounts…`, `POST /api/mail/accounts/:id/sync` | steve-mail E1 | `wired::mail_events_reach_the_signals_page` |
| `/api/spaces…`, `/api/channels/feishu/…` (device flow, user OAuth) | steve-channels C1, F1, F6 | `wired::signal_modules_mount_their_routes` |
| `/api/channels…` (accounts, rules, permissions), `/api/spaces/:id/members`, `POST /api/chan-messages/:id/recall`, `GET /api/chan-messages/:id/basis` | steve-channels capi, C5/C6 | `wired::a_channel_round_trip_on_the_fake_feishu` |
| `GET /api/conversations`, `GET /api/conversations/:space/messages` (IM conversations `chan:<space>`, with each turn's reaction state) | steve-channels §2.12 (`wire/chan.rs`) | `wired::a_channel_round_trip_on_the_fake_feishu` |
| `POST /mcp` tool `steve_reply_source` (not auto-allowed) | C6 | — |
| `POST /api/github/issues/:id/to-item`, `GET /api/github/repos/:id` | steve-github G5 | g5 tests |
| `/api/mail/threads…`, `/api/mail/messages/:id/{html,cid,raw}`, `/api/mail/presets` | steve-mail E3/E6 | steve-mail tests |
| `POST /api/thread {mentions}` → item people (link / infer, “关联了 …”) | contacts P3 via steve-core | `thread_links_mentions_and_infers_people` |
| `POST /mcp` tools `steve_contacts_find`, `steve_contacts_remember` | contacts P3 (layer in `wire/mcp_extra.rs`) | `contacts_tools_are_served_over_mcp` |
| `GET /api/people/:id/holdings`, `GET /api/stewards/unowned`, `GET\|PUT\|DELETE /api/stewards/:type/:id` | contacts ownership (`stewards_api.rs`; edits are changesets, undo via `POST /api/config/:id/undo`) | `ownership_routes_and_tools` |
| `POST /mcp` tools `steve_whois`, `steve_person_holdings`, `steve_steward_propose` | contacts ownership (layer in `wire/mcp_extra.rs`) | `ownership_routes_and_tools` |
| `POST /mcp` tools `steve_knowledge_search`, `steve_knowledge_read`, `steve_remember` | knowledge K3/K4 (same layer) | `knowledge_note_is_found_over_http_and_mcp` |
| `POST /api/config/draft` kinds `schedule`, `contact`, `org`, `team`; apply/undo of stored changesets | steve-modules changes | `module_config_drafts_go_to_the_modules` |
| `general.tz` setting | schedules | `tz_setting` |
