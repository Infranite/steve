//! # External channel over WebSocket: protocol v1
//!
//! Status: version 1, implemented by the Hub's built-in adapter `ext_ws`
//! ([`super`]); §13 lists what this Hub does and does not do yet. This is the reference copy: the Hub
//! owns and publishes this text, and a channel service implements it from this text alone. MUST,
//! MUST NOT, SHOULD and MAY are used as in RFC 2119.
//!
//! ## 0. Terms
//!
//! - **Hub**: the Steve Hub; its built-in adapter `ext_ws` is the **client**. **Channel service**
//!   (**remote**, **server**): an external process bridging one messaging system for one signed-in
//!   user. **Connection**: one Hub `connections` row of adapter `ext_ws` (URL, credential, identity).
//! - **Chat**: a conversation at the remote (DM, group, channel, thread) with a stable `chat_id`, the
//!   Hub space's `ext_id`. **User**: the person signed in at the remote; the Hub acts as them
//!   (`Mode::Own`).
//!
//! ## 1. Roles and direction
//!
//! The channel service is the WebSocket **server**, and the Hub **dials out** to it. The service
//! usually runs on the user's machine or LAN, so a Hub behind NAT can still reach it. The Hub opens
//! no inbound port or route, so a remote can never start a conversation that the Hub did not ask
//! for. Reconnect, backoff and health stay with the supervisor that already runs every connection
//! (`ConnectionHook::run` with its stop token).
//!
//! The user enters `url` and `token` on the credential form in the Hub's settings
//! (`StartMethod::Form`), plus an optional `pin`. The Hub checks the URL when the connection is
//! created and again before every dial:
//! - The scheme MUST be `wss://`. Plain `ws://` is accepted only to a **literal** loopback address
//!   (`127.0.0.0/8` or `::1`). `localhost` and other names do not count, because a name can resolve
//!   anywhere.
//! - The host is read with Steve's strict parser `steve_node_wire::tls::host_of`. That is
//!   `http::Uri`, the parser the dialer itself uses. It refuses userinfo (`@`), a fragment (`#`),
//!   `%` escapes, backslashes, whitespace and control characters anywhere in the URL. The outbound
//!   host guard `steve_modules::net::check_host` applies as for any other provider.
//! - A port and a path are allowed. A query string is refused, because a credential never travels
//!   in a URL.
//! - Trust comes from the webpki roots or from a pin: `sha256:<64 hex>` of the server's leaf
//!   certificate (DER), for a self-signed LAN service. A rotated certificate needs a new pin.
//! - The media origin (§8) is derived from the URL: `wss` becomes `https` and `ws` becomes `http`,
//!   with the same host and port. It is never configured separately.
//!
//! ## 2. Framing and limits
//!
//! - Each frame is one WebSocket **text** frame holding one JSON-RPC 2.0 object (UTF-8, no BOM).
//!   JSON-RPC batch arrays and binary frames are protocol errors. The receiver closes the socket
//!   (code 1003).
//! - The **frame cap is 1 MiB** (1 048 576 bytes) in each direction, unless hello agrees on less. A
//!   larger frame closes the socket (1009). A sender splits its output into pages or smaller
//!   batches.
//! - Request ids are JSON integers, unique among the requester's open requests. Only the Hub sends
//!   requests (§10). The server sends responses and the notifications of §4.6.
//! - Receivers MUST ignore unknown fields. They MUST ignore unknown notification methods, and SHOULD
//!   count them.
//! - The server advertises concrete limits in its hello result (§3.3). The Hub stays below both
//!   those limits and its own ceilings:
//!
//! | Limit | Hub ceiling | Typical server |
//! |---|---|---|
//! | `max_frame_bytes` | 1 MiB | 1 MiB |
//! | `max_text_bytes` (one message body, UTF-8) | 256 KiB | 150 KiB |
//! | `max_batch_items` (items per feed batch) | 200 | 100 |
//! | `max_unacked_batches` (feed window) | 16 | 4 |
//! | `max_ids_per_call` (`messages.get`, `people.get`) | 100 | 50 |
//! | `history_max_limit` (messages per history page) | 200 | 50 |
//! | `write_per_sec` / `write_burst` (writes as the user) | none | 5 / 5 |
//! | `max_attachment_bytes` (HTTP, §8) | the Hub's `max` per download | tens of MiB |
//! | `max_inflight_requests` | 32 | 32 |
//!
//! - **Rate limits.** The Hub paces every write (§4.4) with a token bucket per connection, set to
//!   `write_per_sec` and `write_burst`. A server MAY still answer `rate_limited` with
//!   `data.retry_after_ms`. The Hub then waits at least that long, plus 0–20 % jitter. With no hint,
//!   it backs off exponentially with full jitter, from 1 s up to 60 s. A write is retried only with
//!   the same `idempotency_key`. A read is retried at most 3 times before the error surfaces.
//!
//! ## 3. Authentication and hello
//!
//! ### 3.1 Credential
//!
//! - The credential is a bearer token that the channel service issues per caller, that is, per Hub
//!   connection. It is scoped and revocable. It is sent as `Authorization: Bearer <token>` on the
//!   WebSocket upgrade and on every media request (§8), and never in a URL, a query or a frame.
//! - At upgrade, 401 (unknown or revoked) and 403 (out of scope) both become `ProviderError::Auth`.
//!   The connection goes to reauth, and the Hub does not redial until the credential changes.
//! - A token is 16–4096 bytes of printable ASCII with no whitespace. Anything else is refused at
//!   input.
//!
//! ### 3.2 `hello` request (client → server, the first frame, required)
//!
//! The first frame after the upgrade MUST be the Hub's `hello` request. The server closes the socket
//! (code 1008) if it gets anything else first, or nothing within 10 s. Until hello succeeds, every
//! other method returns `not_ready`.
//!
//! ```json
//! {"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":{"min":1,"max":1},
//!   "client":{"name":"steve-hub","version":"0.2.64"},"time_ms":1762430400000,
//!   "capabilities":["changes","history","media","send","reply","recall","reactions","people"]}}
//! ```
//!
//! ### 3.3 `hello` result
//!
//! ```json
//! {"jsonrpc":"2.0","id":1,"result":{"protocol":1,"server":{"name":"…","version":"…"},
//!   "realm":"acme-7f3a","time_precision_ms":1,"time_ms":1762430400012,
//!   "features":["me","chats.get","messages.get","messages.history","changes","media.download",
//!               "messages.send","messages.reply","messages.recall","reactions","people.get"],
//!   "limits":{"max_frame_bytes":1048576,"max_text_bytes":153600,"max_batch_items":100,
//!             "max_unacked_batches":4,"max_ids_per_call":50,"history_max_limit":50,
//!             "write_per_sec":5,"write_burst":5,"max_attachment_bytes":104857600,
//!             "max_inflight_requests":32},
//!   "session":{"status":"ok"},"upstream":{"status":"ok"},"write_policy":"allowed"}}
//! ```
//!
//! - `realm` is a stable, opaque id for the account's namespace at the remote, such as one tenant or
//!   workspace. It is 1–64 bytes of `[a-z0-9._-]`. User and chat ids are unique within a realm. A
//!   realm that changes between connects is `account_changed`.
//! - `time_precision_ms` (1..=60000) says how precise `created_ms` is. The Hub's `ChannelDecl`
//!   declares 1000 ms. When a remote is coarser than that, its messages are stored with
//!   `created_exact = false`.
//! - `session.status`: `ok` | `expired` | `locked` (§9.2), optional `reason` (≤ 512 chars, shown,
//!   never secret). `upstream.status`: `ok` | `degraded` | `down`, the service's own link to the
//!   messaging system. `write_policy`: `allowed` | `restricted` | `forbidden` (§3.6).
//! - The two `time_ms` values give the clock skew. If it is over 5 min, the Hub logs a warning and
//!   judges lateness (§5.6) by the remote's clock.
//!
//! ### 3.4 Version negotiation
//!
//! - A version is a single positive integer, and this document is version **1**. The client offers
//!   `[min, max]`. The server MUST answer with the **highest version both sides support**: the lower
//!   of the two maxima, provided it is not below the higher of the two minima.
//! - If there is no common version, the server answers `unsupported_version` with
//!   `data.supported: [..]` and closes. The Hub marks the connection `error` and says which side
//!   must upgrade. It redials only at the maximum backoff (5 min), or when the settings change.
//! - Within a version, changes are additive and signalled by feature flags. Both sides ignore
//!   unknown flags, so either side can roll forward first. In major/minor terms, the integer is the
//!   major version and the feature list plays the part of the minor.
//!
//! ### 3.5 Capabilities and features
//!
//! - `capabilities` (client) lists what the Hub can use. A server MAY skip work based on it. A
//!   server MUST NOT refuse a client because it lists something unknown.
//! - `features` (server) lists what it implements. **Required in v1:** `me`, `chats.get`,
//!   `messages.get`, `messages.history`, `changes`, `media.download`; if one is missing the Hub
//!   closes (1000), marks the connection `error` ("the channel service lacks …") and backs off at
//!   the maximum. **Optional:** `chats.list`, `chats.members`, `chats.resolve`, `messages.send`,
//!   `messages.reply`, `messages.edit`, `messages.recall`, `messages.read_state`, `reactions` (add,
//!   remove, list), `people.get`, `changes.inline_body`, `media.upload`, `x.office.*`.
//! - `x.office.*` (people, org, calendar, tasks, docs, approval, mail, search, meetings) is an
//!   **optional extension namespace**. Its method names are reserved, it is out of scope for v1,
//!   and the Hub v1 neither requires nor calls it.
//! - `messages.reply` implies `messages.send`.
//!
//! ### 3.6 Write policy
//!
//! - `allowed`: every write may be attempted.
//! - `restricted`: only some targets accept writes (for example only the user's own chats). The Hub attempts writes normally. A write to a refused target fails with `target_forbidden` (-32016): that one action is `Rejected`, and the connection **stays writable**. Nothing flips to read-only. The UI shows "writes limited by the channel service". Repeated refusals for the same chat are remembered for 24 h, so the Hub doesn't draft replies there.
//! - `forbidden`: see below.
//!
//! If `write_policy` is `forbidden`, the connection is **read-only**. The Hub keeps receiving and
//! shows "read-only: the channel service forbids writes". Every outbound action fails locally with
//! `ProviderError::Rejected`, and nothing is sent. The policy can change mid-session, because
//! `session.status` (§4.6) carries `write_policy`. A write that crosses the change in flight gets
//! `write_forbidden`, and the Hub then behaves as for `forbidden`.
//!
//! ## 4. Methods
//!
//! **R** marks a required method and **O** an optional one. Every request goes from client to
//! server. `chat_id`, `id`, `source_id`, handle values and `cursor` are opaque strings of 1–256
//! bytes with no control characters. They MUST NOT start with `fp:`, because the Hub refuses its
//! own id prefix.
//!
//! ### 4.1 Identity: `me.get` (R)
//!
//! `me.get` takes params `{}`. It maps to `ConnectionProvider` connect and verify, to `AccountInfo`
//! and to `claims_self`.
//!
//! ```json
//! {"realm":"acme-7f3a","user":{"id":{"kind":"id","value":"u_123"},"display":"Ada Lovelace",
//!   "aliases":[{"kind":"stable","value":"x_9ab"},{"kind":"email","value":"ada@example.com"}]}}
//! ```
//!
//! - `user.id` is the **primary handle**, a `Handle` (§6.2). It is stable for this user in this
//!   realm. The `aliases` (at most 8) are a `stable` cross-app id and an `email`.
//! - The Hub calls `me.get` after every hello. On the first connect it builds
//!   `AccountInfo::new(primary, display)` with `.alias(..)` per alias; the adapter `claims_self`, so
//!   all of them are claimed into "me". Later, a primary handle or realm that differs from the stored
//!   one is `account_changed`: the Hub closes and sets the connection to reauth.
//! - Hub-side mapping (not on the wire): the adapter declares handle system `ext_ws`
//!   (`Declare::system`). `id` → (`ext_ws`, `<realm>:id:<value>`); `stable` → (`ext_ws`,
//!   `<realm>:stable:<value>`); `email` → (`mail`, lowercased). Every `Sender` maps the same way.
//!
//! ### 4.2 Chats
//!
//! | Method | | Params | Result | Use in Hub v1 |
//! |---|---|---|---|---|
//! | `chats.get` | R | `{chat_id}` | `Chat` (§6.1) | the space's name and kind on first sight |
//! | `chats.members` | O | `{chat_id, page_token?, limit?}` | `{members:[Member], next_page_token?}` | member count, external flag |
//! | `chats.list` | O | `{page_token?, limit?}` | `{chats:[Chat], next_page_token?}` | not used by Hub v1 |
//! | `chats.resolve` | O | `{user: Handle}` | `{chat_id}`, the DM with that user | not used by Hub v1 |
//!
//! ### 4.3 Reads
//!
//! - **`messages.get` (R).** Params are `{"items":[{"chat_id","source_id"?,"id"?}]}`, with at most
//!   `max_ids_per_call` items. Each item gives at least one of `source_id` and `id`. The result is
//!   `{"messages":[Message|null]}` in the same order, with `null` for a message that cannot be found
//!   or seen. The Hub uses it to fetch the bodies of feed items (§5.3) and for
//!   `MediaFetch::reparse`.
//! - **`messages.history` (R)** is the protocol's history read. It maps to
//!   `HistoryRead::read(cx, space, from_ms, to_ms, limit)` and to gap repair (§5.5). Example params:
//!   `{"chat_id":"c_42","from_ms":1762426800000,"to_ms":1762430400000,"limit":50,"order":"asc",
//!   "page_token":null}`.
//!   The range is `from_ms <= created_ms < to_ms`, both required, at most 31 days; `limit` is
//!   1..=`history_max_limit`; `order` is `asc` (default) or `desc`. The result is
//!   `{"messages":[Message],"next_page_token":"…"|null,"complete":bool}`; `complete: false` means
//!   the remote could not see the whole range (retention, permissions), which the Hub records. A
//!   position range is given by `page_token`, opaque and valid only for the same chat, range and
//!   order (stale: `unknown_cursor`). History is **pull-only**, never pushed, and never new to the
//!   Hub (§5.6).
//! - **`messages.read_state` (O)** takes `{chat_id}` and returns
//!   `{"read_up_to_ms":int|null,"unread":int|null}`. It is informational, and only the Hub's
//!   read-only tools use it.
//! - **`reactions.list` (O)** takes `{chat_id, source_id?, id?}` and returns
//!   `{"reactions":[{"reaction_id","emoji","user":Handle,"is_mine":bool}]}`.
//!
//! ### 4.4 Writes as the user
//!
//! Every write carries two fields:
//! - **`expected_sender`**: the primary `Handle` from `me.get`. If the user signed in at the remote
//!   is someone else, the server MUST refuse with `account_changed`, **before any side effect**.
//! - **`idempotency_key`**: 1–128 bytes of `[A-Za-z0-9._:-]`. It is required on `send`, `reply` and
//!   `edit`, and recommended on the others. The Hub derives it from `SendKey`, which stays the same
//!   across every retry of one outbound message. The server MUST remember keys for at least
//!   **24 h**.
//!   The same key with the same params returns the **original result** with no new side effect;
//!   with different params, `idempotency_conflict`.
//!
//! | Method | | Params (plus the two above) | Result | Hub v1 |
//! |---|---|---|---|---|
//! | `messages.send` | O | `{chat_id, text, mentions?:[Handle]}` | `SendResult` | `ActAsUser::send`, `ActTarget::Space` |
//! | `messages.reply` | O | `{chat_id, to:Target, in_thread:bool, text, mentions?}` | `SendResult` | `ActTarget::Reply{to, in_thread}` |
//! | `messages.edit` | O | `{chat_id, target:Target, text}` | `{edited_ms}` | not used by Hub v1 |
//! | `messages.recall` | O | `{chat_id, target:Target}` | `{}` | `ActAsUser::recall` |
//! | `reactions.add` | O | `{chat_id, target:Target, emoji}` | `{reaction_id}` | `add_reaction` |
//! | `reactions.remove` | O | `{chat_id, target:Target, reaction_id}` | `{}` | `remove_reaction` |
//!
//! - A `Target` is `{"source_id"?, "id"?}`, with at least one of the two. The server resolves
//!   either one.
//! - `SendResult` is `{"source_id":"…","id":"…"|null,"created_ms":int|null,"thread_id":"…"|null}`.
//!   It becomes `Sent{ext_id: id, created: created_ms, thread_key: thread_id, source_id}`.
//!   `source_id` is **required** and is the id the feed later carries for this message: that is how
//!   the Hub recognises the echo and acts on the message again (`MessageRef::source_id`,
//!   `ConnQueries::message_ref_by_source`).
//! - `mentions` are rendered by the service in the platform's own way.
//! - `in_thread: true` answers inside the thread of `to`. `false` posts a quoted reply. A platform
//!   without threads treats `true` as `false` and returns `thread_id: null`.
//! - `emoji` is one Unicode emoji grapheme of at most 32 bytes. The Hub sends 👀 for ack, ✅ for done
//!   and ❌ for failed (`default_emoji`). The service maps each to its closest platform reaction, or
//!   answers `unsupported`.
//! - Repeating a state that is already applied succeeds: recalling a recalled message, or removing
//!   a removed reaction.
//! - The Hub's `ChannelDecl` declares `recall` and `reactions` statically, because a declaration
//!   cannot depend on the remote. When the server lacks a feature, the Hub fails the call locally
//!   with `Unsupported` and sends nothing.
//!
//! ### 4.5 People: `people.get` (O)
//!
//! Params are `{"users":[Handle]}`, with at most `max_ids_per_call` users. It maps to
//! `ProfileAdapter::resolve_profiles`. The result is:
//!
//! ```json
//! {"people":[{"user":{"kind":"id","value":"u_7"},"display":"Grace","email":"grace@example.com",
//!   "avatar_url":"https://…","title":"…","org":"…","aliases":[],"directory":true},null],
//!  "missing_permissions":[{"scope":"…","purpose":"…"}]}
//! ```
//!
//! `null` marks a user the remote cannot see. Missing permissions go to
//! `ProfileAnswer.missing_scopes`, not to an error. `avatar_url` is never fetched with the token.
//!
//! ### 4.6 Feed, status and liveness
//!
//! | Method | Kind | | Purpose |
//! |---|---|---|---|
//! | `changes.subscribe` | request | R | start or resume the feed (§5.1) |
//! | `changes.ack` | request | R | cumulative ack (§5.4) |
//! | `ping` | request | R | `{}` returns `{time_ms}` (§9.1) |
//! | `changes.batch` | notification, server to client | R | one batch of items (§5.2) |
//! | `session.status` | notification, server to client | R | `{status, reason?, write_policy}` (§9.2) |
//! | `push.status` | notification, server to client | R | `{status: ok/degraded/down, since_ms, reason?}`, upstream liveness |
//!
//! The server sends `session.status` or `push.status` on every change of that state, and only then.
//!
//! ## 5. Changes feed
//!
//! ### 5.1 Subscribe
//!
//! The request is `changes.subscribe` with params `{"from":"now"}` or `{"from":{"cursor":"…"}}`.
//! The result is `{"subscription":"s_1","watermark_ms":1762430400100,"resumed":false,"next_seq":1}`.
//!
//! - **`from: "now"`** starts at the present, and `watermark_ms` is the server's time when the
//!   subscription took effect. The server MUST NOT deliver a `message` item with `created_ms` below
//!   `watermark_ms`. Anything older is available only through `messages.history`.
//! - **`from: {cursor}`** resumes right after the batch whose `cursor` the Hub saved, and the result
//!   has `resumed: true`.
//!   `unknown_cursor`: never issued, or another credential's or realm's. `cursor_too_old`: the
//!   position was discarded. A never-acked cursor is not a resume point, so a caller that never
//!   acked starts at now.
//! - A cursor survives a disconnect, a server restart, and a token rotation within the same grant.
//!   Revocation, a realm change or the end of retention invalidates it. Retention SHOULD be
//!   **at least 24 h**.
//! - A socket has at most one subscription. Subscribing again replaces the old one, and the server
//!   sends no more batches of the old one once it has answered.
//!
//! ### 5.2 Batches
//!
//! ```json
//! {"jsonrpc":"2.0","method":"changes.batch","params":{"subscription":"s_1","seq":17,
//!  "cursor":"c:…opaque…","items":[
//!   {"type":"message","chat_id":"c_42","source_id":"ev_88","id":"m_901","created_ms":1762430401234,
//!    "sender":{"id":{"kind":"id","value":"u_7"},"display":"Grace"},"is_mine":false,
//!    "chat_kind":"group","thread_id":null},
//!   {"type":"gap","chats":["c_42","c_77"],"from_ms":1762430300000,"to_ms":1762430400000,
//!    "reason":"upstream reconnect"}]}}
//! ```
//!
//! - `seq` strictly increases within a subscription. It need not be contiguous.
//! - `cursor` (at most 1024 bytes) is the resume point **after** this batch.
//! - A batch has 0..=`max_batch_items` items. An empty batch MAY move the cursor forward.
//! - Items carry **metadata only**. The Hub fetches bodies (text, mentions, attachments) with
//!   `messages.get`. With `changes.inline_body`, the server MAY embed `"message": Message` when it
//!   fits in the frame, and the Hub then skips the fetch.
//! - `is_mine` is **required** on every `message` item. It is true exactly when the sender is the
//!   signed-in user.
//! - Item types: `message` and `gap` (R); `message_recalled` `{chat_id, source_id?, id?}`,
//!   `message_edited` `{…, edited_ms}` and `chat` `{chat_id, change: joined|left|renamed|members}`
//!   (O; → `ConnSink::space_event`). The Hub skips an unknown type but still acks it.
//! - Flow control: at most `max_unacked_batches` batches are outstanding. When the window is full
//!   the server waits; it never drops items silently. If it has to discard, for example when its
//!   buffer overflows, it emits a `gap` for the affected chats.
//!
//! ### 5.3 How the Hub processes a batch
//!
//! 1. If `seq` ≤ `last_acked`, the batch is a redelivery: re-ack it and stop.
//! 2. Fetch the bodies that were not inlined with `messages.get`, in chunks. A `null` body is stored
//!    as metadata only: empty text, labelled "body unavailable".
//! 3. Build each `InboundMessage`. `external_id` is `id`, or `""` if there is none. `source_id` is
//!    the item's `source_id`. The rest follows §6.
//! 4. Pass each message to `ConnSink::message`. The Hub drops a delivery whose `source_id` this
//!    connection has already stored, and anything `ConnQueries::known` reports. This is the dedupe.
//! 5. Handle `gap` items (§5.5).
//! 6. `ConnQueries::save_cursor("changes", cursor)`, then `changes.ack`.
//!
//! A batch is acked only after it is stored, so the Hub loses nothing. If the Hub crashes between
//! steps 4 and 6, the server redelivers the batch and step 4 drops the duplicates.
//!
//! ### 5.4 Ack
//!
//! The request is `changes.ack {"subscription":"s_1","seq":17}`, and it returns `{}`.
//!
//! - An ack is **cumulative**: it covers every batch with `seq` ≤ 17.
//! - The Hub acks each batch once it is stored. It MAY coalesce acks for up to 1 s, but never so
//!   long that the window stalls.
//! - The server redelivers unacked batches, with the **same seq, cursor and items**, after an ack
//!   timeout (SHOULD be 30 s) or on resume.
//! - An ack above the highest seq sent is `invalid`. An ack for an old seq succeeds as a no-op.
//!
//! ### 5.5 Gaps
//!
//! A `gap` item says that items **may** have been lost for the listed `chats` in
//! `[from_ms, to_ms)`. `["*"]` means every chat. The protocol never back-fills on its own; the Hub
//! repairs the gap:
//!
//! - It reads `messages.history` over the window for each listed chat that the connection covers
//!   (`ConnQueries::covered`), or for every covered chat when the list is `*`. It reads at most 10
//!   pages of 50 messages per chat per gap.
//! - It stores what it finds **as backfill**. Backfill is recorded only (§5.6) and is deduplicated
//!   by `source_id`, then by `id`.
//! - If it could not repair the whole window, it records one line on the space saying that messages
//!   between X and Y may be missing.
//!
//! ### 5.6 Watermark, lateness and own messages (Hub rules)
//!
//! The Hub **records only** a message (no reply, no flow, no draft) when any of these holds:
//! - `created_ms` is before the connection's **enable time**, or before the subscription's
//!   `watermark_ms`;
//! - the message arrives more than the configured **lateness** after `created_ms`;
//! - it came from `messages.history` or from gap repair (backfill);
//! - it is the user's own message. That is the case when `is_mine` is true, which the Hub labels
//!   `Next::Own`, "own message". It is also the case when the Hub's party check finds the sender's
//!   handles claimed as "me" (`claims_self`), which backs up a wrong `is_mine`.
//!
//! **The owner's own messages never get replies.**
//!
//! ### 5.7 Resume after a disconnect
//!
//! 1. Dial, send `hello`, then `me.get`. If the identity differs, stop with `account_changed`.
//! 2. Read the saved cursor with `ConnQueries::cursor("changes")`. If there is one, subscribe with
//!    `{from:{cursor}}`. Otherwise subscribe with `{from:"now"}`.
//! 3. On `unknown_cursor` or `cursor_too_old`: drop the cursor, subscribe `from:"now"`, and treat
//!    `[t_last, watermark_ms)` as a gap for every covered chat (§5.5). `t_last` is when the last
//!    cursor was saved, kept beside it. Everything found is backfill.
//!
//! ## 6. Shapes
//!
//! All times are integer **milliseconds since the Unix epoch, UTC**, at the precision of
//! `time_precision_ms`. A receiver refuses times before 2000-01-01, or more than 24 h in the future.
//! Required strings are never null.
//!
//! **Chat id stability.** A server SHOULD use, as `chat_id`, the identifier the messaging platform's own bot or API uses for that chat where one exists. It falls back to its own stable id otherwise, for example for direct chats. Then the same chat seen through another adapter (such as a bot connection on the same platform) lines up as one space in the Hub.
//!
//! **Chat.** `{"chat_id":"c_42","kind":"group","name":"Design","member_count":12,
//! "has_external":false}`. `kind` is `dm` | `group` | `channel` | `thread` (`SpaceKind`), `name`
//! ≤ 256 chars, `member_count` may be null.
//!
//! **Member.** `{"user":Handle,"display":"…","role":"owner"|"admin"|"member","external":bool}`.
//!
//! **Handle.** `{"kind":"id"|"stable"|"email"|"platform","value":"…","system"?:"…"}`. `value` is 1–256 bytes, and an
//! `email` value contains exactly one `@`.
//! - `platform` (optional, aliases only): an id in a **named system the Hub already knows**, given by `system` (`[a-z0-9._-]`, 1–64 chars), for example the system a built-in bot adapter on the same platform stores its users under. If `system` is a seeded or declared handle system in the Hub, the Hub stores the alias under that system as is, so the same person matches across adapters. If the Hub doesn't know `system`, it ignores the alias (no error). A `platform` handle is never a primary handle.
//!
//! **Sender.** `{"id":Handle,"display":"…"?,"is_bot":false,"external":false,"aliases":[Handle]}`.
//! - `id` is the primary handle, of kind `id` or `email`. There are at most 8 aliases.
//! - The Hub builds `Sender::new(primary).alias(..).display_name(..).bot(..).external(..)` with the
//!   mapping of §4.1. It matches an existing party by the primary handle first, then by each alias.
//!
//! **Message.**
//!
//! ```json
//! {"chat_id":"c_42","source_id":"ev_88","id":"m_901","created_ms":1762430401234,"edited_ms":null,
//!  "sender":Sender,"is_mine":false,"chat_kind":"group","chat_name":"Design","thread_id":"t_5",
//!  "reply_to":{"source_id":null,"id":"m_880"},"root_id":"m_870","kind":"text",
//!  "text":"hello @Ada","mentions":[{"user":Handle|null,"name":"Ada","is_me":true}],
//!  "attachments":[{"type":"image","key":"k_abc","name":"a.png","size":20480,"mime":"image/png"}],
//!  "recalled":false,"raw":{}}
//! ```
//!
//! - `source_id` is required. It is the stable per-message delivery id, and it is the same in
//!   redeliveries, in history and in `SendResult`. `id` is the platform's message id, if the
//!   platform has one.
//! - To `InboundMessage`: `thread_id`→`thread_key`, `reply_to.id`→`reply_to`, `root_id`→`root_id`,
//!   `text` (plain, mentions as `@name`)→`text`, `mentions[].is_me`→`Mention.is_connection`,
//!   `attachments`→`[{type,key,name,size,mime}]`, `raw` (optional, ≤ 64 KiB, opaque)→`raw`. `via`
//!   is `ws`, or `history` for a history read.
//! - `kind` is `text`, `rich`, `system` or `unknown`. For anything other than `text`, `text` is a
//!   plain rendering or a short description.
//! - `attachments[].type` is `image`, `file`, `audio` or `video`. `key` (1–512 bytes) is the media
//!   key (§8), and it is valid only together with this message.
//!
//! ## 7. Errors
//!
//! An error is `{"code":int,"message":"…","data":{"reason":"<name>",…}}`. `data.reason` is the
//! normative value, and `code` follows from it. `message` is at most 512 characters and is shown to
//! the user, so it MUST NOT contain secrets or upstream bodies.
//!
//! | reason | code | Retry | Idempotency | What the Hub does |
//! |---|---|---|---|---|
//! | `parse_error` | -32700 | no | n/a | protocol bug: log, close, reconnect with backoff |
//! | `invalid` | -32602 | no | n/a | `Rejected`; logs the shape of the params, never their values |
//! | `unsupported` | -32601 | no | n/a | `ProviderError::Unsupported`; the feature counts as absent for the session |
//! | `internal` | -32603 | reads up to 3×; writes only with the same key | key ⇒ at most once | `Transient` |
//! | `not_ready` | -32000 | after hello | n/a | protocol bug: close, reconnect |
//! | `unsupported_version` | -32001 | no | n/a | connection `error` (§3.4) |
//! | `unauthorized` | -32002 | no | n/a | `Auth`: reauth, close, no redial until the credential changes |
//! | `session_expired` | -32003 | no | no side effect | `Auth`: shows reauth, keeps the socket, waits for `session.status ok` |
//! | `session_locked` | -32004 | after `session.status ok` | no side effect | degraded ("the channel service is locked"); keeps the socket |
//! | `account_changed` | -32005 | no | refused before any side effect | `Auth`: reauth, close; keeps the cursor until the user re-binds |
//! | `write_forbidden` | -32006 | no | no side effect | read-only (§3.6); the action is `Rejected` |
//! | `rate_limited` | -32007 | after `data.retry_after_ms` | same key, no duplicates | backoff with jitter (§2); `Transient` once retries run out |
//! | `not_found` | -32008 | no | n/a | `Rejected`; for media, `NotFound` (`Recalled` if `data.recalled`) |
//! | `forbidden` | -32009 | no | n/a | `Rejected`; for media, `NoPermission`; for people, a missing permission |
//! | `unknown_cursor` | -32010 | resubscribe | n/a | §5.7 step 3 |
//! | `cursor_too_old` | -32011 | resubscribe | n/a | §5.7 step 3 |
//! | `too_large` | -32012 | no | n/a | `Rejected`; for media, `TooLarge` |
//! | `idempotency_conflict` | -32013 | no | the key was used for other params | Hub bug: `Rejected`, logged |
//! | `upstream_unavailable` | -32014 | with backoff | key-protected | `Transient`; shown as degraded, not as an error |
//! | `timeout` | -32015 | reads yes; writes with the same key | key-protected | `Transient` |
//! | `target_forbidden` | -32016 | no | no side effect | the action is `Rejected`; the connection stays writable (§3.6 `restricted`) |
//!
//! - **Hub-side timeouts.** Hello and ping: 10 s. Reads and writes: 30 s. A history page: 60 s.
//!   When a write times out, its outcome is unknown, so the Hub retries it only with the same
//!   `idempotency_key`.
//! - `session_expired` and `account_changed` need the user; `upstream_unavailable` does not. They
//!   are deliberately distinct, and the Hub never reconnects in a loop over the first two.
//!
//! ## 8. Media over HTTP
//!
//! - **Download.** `GET <origin>/v1/media/<url-escaped key>?chat_id=…&source_id=…` with
//!   `Authorization: Bearer <token>`. This implements `MediaFetch::download(cx, msg, key, image,
//!   max)`.
//! - **Authorization: the same bearer, same origin only.** This is recommended and adopted for v1.
//!   Signed URLs are not used, because they turn a credential's power into a string that logs and
//!   proxies capture. The Hub never sends the token to another origin, follows no redirects, and
//!   treats any 3xx as an error.
//! - **Size.** The Hub sends `X-Max-Bytes: <max>`. The server answers 200 with `Content-Type` and
//!   `Content-Length`, or 413 with no body when the file is over `max` or `max_attachment_bytes`.
//!   The Hub stops reading at `max` bytes, whatever the headers say. The download timeout is 120 s.
//! - **Status → `MediaError`:** 404 `NotFound`; 410 `Recalled` or `Gone` (by the JSON body's
//!   `reason`); 403 `NoPermission`; 413 `TooLarge`; 429 `Busy` (honours `Retry-After`); 5xx or
//!   timeout `Timeout`. A 401 puts the connection into `Auth`.
//! - **Content types.** Any type may be downloaded. The Hub sniffs the content, and stores active
//!   types (`text/html`, `image/svg+xml`, `application/javascript`) as files, never as images.
//! - **Upload** (`media.upload`, optional, unused by Hub v1). `POST <origin>/v1/media` with the same
//!   bearer, a raw body and `Content-Type`. It returns `{"key"}`, which can be used in a write for
//!   1 h.
//!
//! ## 9. Health and liveness
//!
//! ### 9.1 Ping and silence
//!
//! The Hub sends `ping` every **20 s** and answers WebSocket ping control frames. The server drops a
//! client after about **60 s** without a frame. The Hub drops the socket and reconnects after
//! **60 s** without a frame from the server; ping responses count.
//!
//! ### 9.2 Session and upstream status
//!
//! | State | Seen in | Hub connection | Socket |
//! |---|---|---|---|
//! | session `ok`, upstream `ok` | hello, notification | ok | kept |
//! | upstream `degraded` / `down` | `push.status` | **degraded**: a warning; gaps expected | kept |
//! | session `locked` | hello, `session.status` | degraded: "the channel service needs unlocking" | kept; not subscribed until `ok` |
//! | session `expired` | hello, `session.status`, error | **reauth**: "sign in again in the channel service" | kept; no subscription or writes until `ok` |
//! | `unauthorized`, `account_changed` | upgrade, error | reauth | closed; no redial until the user acts |
//! | socket down | any | **error** after 3 failed dials in a row | backoff |
//!
//! - When a session goes from `expired` to `ok`, the Hub runs `me.get`. If the identity is the same,
//!   it resubscribes from the cursor. If it differs, that is `account_changed`.
//! - `ConnectionProvider::health` reports `Health::Auth` for the reauth rows and `Health::Failed`
//!   for the error row. A degraded connection is not a failure.
//!
//! ### 9.3 Isolation: a remote never affects the Hub
//!
//! - **Isolation.** Each connection runs in its own `ConnectionHook::run` task. Every wait is
//!   bounded by the timeouts in §7 and by the stop token, and the supervisor's 30 s abort on
//!   restart is the backstop.
//! - **Bounded buffers:** at most `max_unacked_batches` (≤ 16) queued batches and
//!   `max_inflight_requests` open requests; oversized frames close the socket. Back-pressure is not
//!   acking, so nothing a remote sends grows the Hub's memory without bound.
//! - **Reconnect.** Backoff is exponential with full jitter, from 1 s doubling up to 5 min. It
//!   resets after 60 s connected with a successful hello. A refused upgrade (401/403) or
//!   `unsupported_version` waits the maximum.
//! - **A remote crash** costs liveness only. The cursor lives in the Hub's store, so the next
//!   connection resumes, and any loss arrives as a gap.
//!
//! ## 10. Security
//!
//! - **Token storage.** The token is stored in the Hub's **vault** as the connection's credential.
//!   It never appears in logs, events, API output, error text or `raw`; logs show only
//!   `ext_ws:<connection id>`. Rotating the token means editing the connection, and the cursor
//!   survives if the server keeps the grant.
//! - **TLS** is required off loopback (§1), verified by webpki or a pin, and never disabled.
//! - **No server-initiated commands.** The server only answers requests and sends the
//!   notifications of §4.6; a server request gets `-32601` and is ignored. Nothing in a frame makes
//!   the Hub send, reply, react, recall, fetch a URL or change a setting: outbound actions come only
//!   from the Hub's outbox and rules. Message text is data, never an instruction to the adapter.
//! - **Validation.** Every field is checked: type, the bounds of §4–§6, enums, time bounds, array
//!   sizes, the `fp:` prefix. A malformed item is dropped and logged and the rest processed. A frame
//!   over a limit (bytes, items, ids) is a protocol violation: not acked, socket closed. A frame that
//!   is not JSON-RPC closes the socket.
//! - **Writes** are protected by `expected_sender` and `idempotency_key` (§4.4).
//! - **Media** never carries the token to another origin (§8). URLs in content, such as
//!   `avatar_url`, are never fetched with the token.
//!
//! ## 11. Conformance
//!
//! ### 11.1 Server checklist
//!
//! - [ ] Upgrade without a valid bearer → 401/403. No hello within 10 s, or another first frame → close 1008.
//! - [ ] Negotiates the highest common version; returns `unsupported_version` with `data.supported` when there is none.
//! - [ ] Hello result has `realm`, the required `features`, `limits`, `time_precision_ms`, `session`, `upstream` and `write_policy`.
//! - [ ] Ignores unknown capabilities and fields. Never sends requests, batch arrays or binary frames.
//! - [ ] `from:"now"` delivers nothing created before `watermark_ms`. Never back-fills.
//! - [ ] `seq` strictly increases. A resume continues after the acked batch, and redelivery keeps the same seq and cursor. Never more than `max_unacked_batches` outstanding.
//! - [ ] Every `message` item has `source_id` and `is_mine`. `source_id` is the same in feed, `messages.get`, `messages.history` and `SendResult`.
//! - [ ] Emits `gap` whenever loss is possible, including after its own restart or a buffer overflow.
//! - [ ] Checks `expected_sender` before any side effect. Keeps idempotency keys for 24 h.
//! - [ ] Keeps `session_expired`, `session_locked`, `account_changed` and `upstream_unavailable` distinct. Pushes `session.status` and `push.status` on every change.
//! - [ ] Media: same origin, bearer, 413 over the cap, no redirect to another origin.
//!
//! ### 11.2 Hub checklist
//!
//! - [ ] Refuses `ws://` except to a literal loopback IP. Refuses URLs containing `@`, `#`, `%`, `\`, a query, whitespace or control characters.
//! - [ ] Hello first, then `me.get` and the identity check.
//! - [ ] Saves the cursor before it acks. Dedupes by `source_id`. A seq ≤ last acked is a redelivery.
//! - [ ] Records only: messages before the enable time or watermark, late messages, backfill, and own messages (`is_mine` or the party match). Never replies to the owner's own messages.
//! - [ ] Never logs the token. Bounded queues, a timeout on every request, backoff with jitter.
//! - [ ] Read-only under `forbidden`. Reauth on `unauthorized`, `account_changed` and `session_expired`, with no reconnect loop.
//!
//! ### 11.3 Test vectors
//!
//! `→` is Hub to server, `←` is server to Hub. The Hub ships these vectors as a fake channel
//! service in its test suite (`steve_channels::fake::ext_ws`, driven by
//! `hub-rs/crates/steve-channels/tests/ext_ws.rs`), and a server can replay the `→` side against
//! itself.
//!
//! **V1, version negotiation.** Client `[1,1]` with server `{1}` gives 1. `[1,3]` with `{1,2}`
//! gives 2. `[1,1]` with `{1,2}` gives 1. `[2,3]` with `{1}` gives `unsupported_version` with
//! `supported: [1]`.
//!
//! **V2, URLs.** Accepted: `wss://chat.example.com/ws`, `ws://127.0.0.1:7788/ws`,
//! `ws://[::1]:7788/`. Refused: `ws://localhost:7788/`, `ws://192.168.1.5/` (plain but not to a
//! literal loopback), `wss://evil#@127.0.0.1/`, `wss://a.example/ws?token=x`, `wss://a.example/%2e`,
//! `https://a.example/`.
//!
//! **V3, first frame.** → `me.get` before hello. ← `not_ready` or close 1008. With no frame at all,
//! the server closes after 10 s.
//!
//! **V4, watermark.** ← `watermark_ms: 1000`. A buffered message with `created_ms: 999` never
//! appears in a batch, and one with `created_ms: 1001` does.
//!
//! **V5, redelivery.** ← seq 5 with `source_id: "ev_1"`. The Hub stores it and the ack is lost. ←
//! seq 5 again after 30 s. Exactly one message is stored, and the Hub acks 5.
//!
//! **V6, resume.** The Hub saved the cursor of seq 9 and disconnected. → `subscribe {from:{cursor}}`.
//! ← `resumed: true`, and nothing from seq ≤ 9 follows.
//!
//! **V7, cursor too old.** → `subscribe {from:{cursor:"old"}}`. ← `cursor_too_old`. → `subscribe
//! {from:"now"}`, then `messages.history` for each covered chat over `[t_last, watermark_ms)`. All
//! of it is stored as backfill, recorded only.
//!
//! **V8, gap.** ← `{"type":"gap","chats":["c_42"],"from_ms":100,"to_ms":200}`. →
//! `messages.history {chat_id:"c_42",from_ms:100,to_ms:200,limit:50}`. What it finds is recorded
//! only, and a message the Hub already has by `source_id` is not duplicated.
//!
//! **V9, own message.** ← `is_mine: true`, mentioning the user. It is stored as "own message", with
//! no draft or reply. The same item with `is_mine: false` from a handle claimed as "me" gives the
//! same result.
//!
//! **V10, account changed.** → `messages.send {expected_sender:{"kind":"id","value":"u_123"},…}`
//! while `u_999` is signed in. ← `account_changed` (-32005), and nothing is sent. The Hub sets
//! reauth and closes.
//!
//! **V11, idempotent send.** → `messages.send {idempotency_key:"k1",…}` twice. ← the same
//! `SendResult` both times, and one message at the remote. → `k1` with different text. ←
//! `idempotency_conflict`.
//!
//! **V12, rate limit.** → 10 queued sends with `write_per_sec: 5` are paced at ≤ 5/s. After ←
//! `rate_limited {retry_after_ms: 800}`, the retry comes ≥ 800 ms later with the same key.
//!
//! **V13, session expired.** ← `session.status {status:"expired"}`. The Hub shows reauth, keeps
//! pinging and sends no writes. ← `{status:"ok"}`. → `me.get`, then `subscribe` from the cursor.
//!
//! **V14, write forbidden.** With `write_policy: "forbidden"` in hello, a reply is refused inside
//! the Hub, and the server sees no write frame.
//!
//! **V15, limits.** ← a frame of 1 MiB + 1 byte: the Hub closes with 1009. ← a batch of
//! `max_batch_items + 1` items: the Hub does not ack it, logs it and closes.
//!
//! ## 12. Versioning policy
//!
//! - **Additive** changes keep the version and add a feature flag: a new optional method,
//!   notification, item type, field, limit or error `reason`. Receivers ignore what they do not
//!   know. A Hub treats an unknown `reason` as `internal`. Each new flag is listed in this document
//!   with the date it was added.
//! - **Breaking** changes bump the integer. That means removing or renaming anything, changing a type
//!   or a meaning, moving a feature into the required set, or tightening a limit below the Hub's
//!   ceilings. A server SHOULD keep speaking the previous version for at least 6 months after the
//!   first Hub release that speaks the new one. The Hub keeps every version that a released server
//!   still speaks.
//! - `x.` namespaces (`x.office.*`, `x.<vendor>.*`) evolve under their own flags and never affect
//!   conformance.
//! - This text lives beside the adapter as rustdoc, which is the reference copy. A spec change and
//!   the matching change to the Hub's conformance vectors land in the same commit.
//!
//! ## 13. What this Hub implements
//!
//! The built-in adapter `ext_ws` implements v1 as written above, with these exceptions. Each is on
//! the Hub's side only; a conforming server needs nothing different.
//!
//! - **Session `expired` and `locked`** are shown on the connection as a failure ("sign in again in
//!   the channel service", "the channel service needs unlocking"), not as the Hub's `reauth` status:
//!   `reauth` stops the connection's feed, and the feed is what waits for `session.status ok`. The
//!   socket stays, nothing is taken from the feed and no write goes out until the session is `ok`
//!   again; then `me.get` runs and the feed resumes from the cursor (§9.2).
//! - **Clock skew** over 5 minutes is logged. Lateness (§5.6) is judged by the Hub's clock.
//! - **Acks** are not coalesced: each batch is acked once it is stored (§5.4).
//! - **Gaps**: `["*"]` reads every chat the connection has (the 200 most recently active). A window
//!   that could not be read whole becomes one line in the Hub's next report, not a line on the
//!   space (§5.5).
//! - **`restricted`**: a refused write (`target_forbidden`) is rejected and the connection stays
//!   writable; refusals are not yet remembered per chat to stop drafting replies there (§3.6).
//! - **Item types** `message_recalled`, `message_edited` and `chat` with `change: members` are
//!   skipped and acked; `chat` `joined`, `left` and `renamed` are applied (§5.2).
//! - **Not called by Hub v1**: `chats.list`, `chats.members`, `chats.resolve`, `messages.edit`,
//!   `messages.read_state`, `reactions.list`, `media.upload`, `x.office.*`.
//! - **A body that cannot be fetched** (`null` from `messages.get`) is stored with empty text and
//!   `raw: {"body_unavailable": true}` (§5.3).
//! - **Media**: the size cap, same origin, bearer and no redirects are the adapter's; how a file is
//!   stored (image or file) is the Hub's media pipeline's (§8).

//!
//! ## 14. Generic connector extension (2026-11-14)
//!
//! A v1 hello MAY include `connector: {version: 1, id, operations, events}`. This extension needs
//! only `me` and `changes`; other features remain independent. Legacy bridges without this
//! declaration keep the required messaging feature set and history gap repair. Connector mode
//! starts at now/resumes its cursor, but never repairs gaps by importing history automatically.
//!
//! `operations` (at most 64) are MCP `{name,description,inputSchema,annotations}` plus `capability`
//! (`search|history|read|write`) and `bindings` (top-level argument ->
//! `conversation|message|topic|reference`). Schemas are closed objects, bound fields are strings,
//! and non-search operations require a bound target. Names are ASCII alphanumeric/underscore/hyphen,
//! at most 64 bytes; the Hub derives the exported, connection-scoped tool name itself. Reads must
//! set `readOnlyHint:true`; writes set false. `annotations.steve` may declare `resources` and
//! `asYou`. Declarations are metadata, not authorization. Every call rechecks current caller,
//! account/connection and schema, then resolves Hub-issued IDs to that account's provider IDs.
//!
//! `connector.call {name,arguments,expected_sender?,idempotency_key?}` returns JSON. Writes require
//! the connection's send-as-you capability, current signed-in identity and write policy. The Hub
//! sends once; a timeout or lost socket returns `{status:"unknown",request_id, retry:false}`.
//! There is no automatic retry, including rate-limit responses. The service must check
//! `expected_sender` before side effects and implement idempotency-key conflict detection (§4.4).
//!
//! Results may include `references` (at most 200), each with `kind` (a binding kind), `external_id`,
//! optional `conversation_id`, `source_message_id`, required `original_type`, and optional checked
//! title/name/size/MIME/HTTP(S) URL/`expires_ms`. The Hub registers an account/connection-scoped
//! `reference_id`, never accepting a plugin-chosen Hub ID. Defaults expire after one day, maximum
//! 30 days. Expired/missing references are errors, not permission to guess a provider ID. Reads
//! are explicit and may return original content; registering references never imports history or
//! triggers historical participants/automation. The Hub retains provenance/type, not inline bytes.
//!
//! Conversation kinds are `dm|group|channel|thread|mail_thread|meeting|pr|issue`. Message
//! `source_id`, platform `id`, `thread_id`, `reply_to.id` and `root_id` retain their existing roles.
//! Attachments additionally support `document|webpage|commit|log|audio|video`; unknown fields and
//! inline body/HTML/bytes are dropped. The Hub adds a deterministic per-message `reference_id`.
//! `participants` batch items contain `conversation_id`, `change:added|removed`, and `members`
//! (at most 256), whose IDs/aliases use the sender shape. They update the common membership model.
//!
//! Declared `events` (at most 64 type keys) arrive as batch items
//! `{type:"event",event_type,event_id,occurred_ms,summary,conversation_id?,topic_id?}`. Stable
//! `event_id` is deduplicated by the ordinary Hub event sink under the authenticated connection;
//! the batch cursor is saved only after ingestion, then acked. The source is `connector`; event
//! privacy comes from the connection account. Remote Hub IDs, visibility or flow controls are
//! ignored. Undeclared event types are skipped and acked. Platform-specific payloads stay in the
//! plugin and can be fetched through a declared explicit read operation.
