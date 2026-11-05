# Feishu sample payloads

These are plaintext schema-2.0 callback bodies, in the shape they have after decryption. They were written from the field lists and examples in the open-platform docs; they are not recordings. They are to be replaced with real callbacks recorded during a live hookup.

The ids match `World::sample()` in `src/fake/feishu/model.rs`:

| Field | Value |
| --- | --- |
| App ID | `cli_a5fake0000000001` |
| Tenant key | `2ca1d211f64f6438` |
| External tenant key | `736588c9260f175d` |
| Verification Token | `fake-verification-token` |
| Encrypt Key | `fake-encrypt-key` |
| Bot open_id | `ou_bot0000000000000000000000000001` |

To send one through the fake, call `EventPusher::push_envelope(fixture("…"))`. Pushing the same fixture twice is a duplicate delivery, because the `event_id` is unchanged.

| File | Case |
| --- | --- |
| `message_p2p_text` | Private text to the bot (`chat_type=p2p`) |
| `message_group_at_bot` | Group text that @-mentions the bot (`@_user_1`) |
| `message_group_plain` | Group text with no mention |
| `message_post_rich` | `post` rich text with an @bot, a link, and an @owner |
| `message_image` / `message_file` | Image or file; the attachment bytes are served from `resources/{file_key}` |
| `message_thread_reply` | Reply inside a topic thread (`root_id`, `parent_id`, `thread_id`) |
| `message_from_bot` | Sent by another app (`sender_type=app`) |
| `message_external_user` | Sent by a member of another tenant in the external group |
| `bot_added` / `bot_deleted` | Bot added to a new group, or removed from one |
| `user_added` / `user_deleted` | External user joins; internal user leaves |
| `chat_updated` | Group renamed (`after_change` / `before_change`) |
| `chat_disbanded` | Topic group disbanded |
| `contact_user_updated` / `contact_user_resigned` | `contact.user.updated_v3`, once with a job-title change and once with `is_resigned` flipped |
| `contact_department_deleted` | `contact.department.deleted_v3` |
| `url_verification_encrypted` | Encrypted challenge, plus a signature computed with fixed timestamp and nonce, the plaintext, and the expected response |
| `crypto_doc_vector` | The decryption example from the docs: key `test key`, plaintext `hello world` |

## Sources (checked 2026-11-01)

The doc pages are rendered with JS. Their raw markdown, including the example blocks, can be fetched from `https://open.feishu.cn/document_portal/v1/document/get_detail?fullPath=<path>`.

- **Event delivery, envelope, 3 s timeout, retries (15 s / 5 min / 1 h / 6 h), event_id dedup:** https://open.feishu.cn/document/ukTMukTMukTM/uUTNz4SN1MjL1UzM
- **Signature, AES-256-CBC decryption, and the `test key` / `P37w+VZImNgPEO1RBhJ6RtKl7n6zymIbEG1pReEzghk=` → `hello world` vector:** https://open.feishu.cn/document/server-docs/event-subscription-guide/event-subscription-configure-/encrypt-key-encryption-configuration-case
- **URL verification (`challenge`, `token`, `type`; the docs exclude it from signing):** https://open.feishu.cn/document/ukTMukTMukTM/uYDNxYjL2QTM24iN0EjN/event-subscription-configure-/choose-a-subscription-mode/send-notifications-to-developers-server
- **`im.message.receive_v1`:** https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message/events/receive
- **Received message content (text, post, image, file):** https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/im-v1/message/events/message_content
- **Bot added or removed:** https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/chat-member-bot/events/added (the `deleted` page is at the same path, ending `/deleted`)
- **User added or removed:** https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/chat-member-user/events/added (the `deleted` page is at the same path, ending `/deleted`)
- **Chat updated:** https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/chat/events/updated
- **Chat disbanded:** https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/chat/events/disbanded
- **`contact.user.updated_v3`:** https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/contact-v3/user/events/updated
- **`contact.department.deleted_v3`:** https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/contact-v3/department/events/deleted

The fake's REST responses follow these pages:

- **tenant_access_token:** https://open.feishu.cn/document/ukTMukTMukTM/ukDNz4SO0MjL5QzM/auth-v3/auth/tenant_access_token_internal
- **Generic error codes (99991661/63/68/72/77, 99991400):** https://open.feishu.cn/document/ukTMukTMukTM/ugjM14COyUjL4ITN
- **Rate limits (HTTP 429, `x-ogw-ratelimit-limit`, `x-ogw-ratelimit-reset`):** https://open.feishu.cn/document/server-docs/api-call-guide/frequency-control
- **bot info:** https://open.feishu.cn/document/ukTMukTMukTM/uAjMxEjLwITMx4CMyETM
- **Messages, under `https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/`:**
  - `message/create`
  - `message/reply`
  - `message/delete`
  - `message/list`
  - `message-resource/get`
- **Chats, under the same prefix:**
  - `chat/get`
  - `chat-members/get`
  - `chat/list`
- **Contact, under `https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/contact-v3/`:**
  - `scope/list`
  - `department/children`
  - `user/find_by_department`
- **User OAuth token (flat response, `{code, error, error_description}` on failure):** https://open.feishu.cn/document/authentication-management/access-token/get-user-access-token

## Not confirmed by the docs (check in F7)

- **Signature test vector:** the docs give no numeric example. `crypto.rs` pins the formula against `shasum`.
- **Redelivered `event_id`:** the docs imply that a redelivery keeps the same `event_id` (they recommend deduplicating on it) but never state it.
- **Bot mentions:** the shape of a mention of the bot is assumed. These samples use `mentioned_type: "bot"` with empty `union_id` and `user_id`; the docs' example only shows `"user"`.
- **HTTP status for token errors:** the fake returns 400.
- **Some error codes:** `230001`, `230011`, `40004`, `41050`, `42002`, `10014`, `99992402` and `20001`–`20026` were not checked one by one.

## Long connection and user identity
- Long connection: larksuite/oapi-sdk-go `ws/` at db9f52d (2026-10-15) — `pbbp2.pb.go` (frame
  layout; gogo always writes fields 6, 7, 9), `client_transport.go` (endpoint, handshake errors),
  `client_session.go` / `client_lifecycle.go` (ping, read deadline `2×PingInterval+5 s`,
  reconnect), `client_message.go` (reassembly, ack payload). The long-connection redelivery
  schedule and whether events are encrypted there are unknown (F7); the fake defaults to the
  webhook schedule and plaintext.
- User identity: larksuite/cli at c910e89 — `internal/auth/device_flow.go` (device flow),
  `internal/errclass/codemeta.go` (20026/20037/20064/20073, 99991679),
  `internal/event/consume/remote_preflight.go` (`GET /open-apis/event/v1/connection`),
  `shortcuts/im/*` (chat list `types`, search body, mget). `codes::USER_IDENTITY_SHARED_APP` and
  `IM_SEND_RATE_LIMITED` (230020) are not confirmed by a source; check in F7.
