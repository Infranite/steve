//! Error codes and messages the fake returns. Values from the open-platform docs; see
//! `tests/fixtures/feishu/README.md` for the sources.

/// Missing `Authorization` header.
pub const MISSING_TOKEN: i64 = 99991661;
/// Tenant token invalid or expired (Feishu uses one code for both).
pub const INVALID_TOKEN: i64 = 99991663;
/// User token invalid.
pub const USER_TOKEN_INVALID: i64 = 99991668;
/// User token expired.
pub const USER_TOKEN_EXPIRED: i64 = 99991677;
/// Gateway frequency limit.
pub const RATE_LIMITED: i64 = 99991400;
/// App lacks a required scope.
pub const NO_SCOPE: i64 = 99991672;
/// Request parameter validation failed.
pub const FIELD_VALIDATION: i64 = 99992402;
/// Wrong app_id / app_secret.
pub const BAD_APP_SECRET: i64 = 10014;
/// im: invalid request parameters (e.g. content not JSON).
pub const IM_INVALID_PARAM: i64 = 230001;
/// im: bot is not in the chat.
pub const IM_BOT_NOT_IN_CHAT: i64 = 230002;
/// im: message not found / already recalled.
pub const IM_MESSAGE_NOT_FOUND: i64 = 230011;
/// im: only the sender may recall.
pub const IM_NOT_SENDER: i64 = 230026;
/// im: message already recalled.
pub const IM_MESSAGE_DELETED: i64 = 230110;
/// im: resource not found.
pub const IM_RESOURCE_NOT_FOUND: i64 = 234003;
/// chats: invalid chat_id.
pub const CHAT_NOT_FOUND: i64 = 232006;
/// chats: chat disbanded.
pub const CHAT_DISBANDED: i64 = 232009;
/// contact: no permission for this department.
pub const CONTACT_NO_DEPT_AUTHORITY: i64 = 40004;
/// contact: the user is outside the app's contact scope.
pub const CONTACT_NO_USER_AUTHORITY: i64 = 41050;
/// contact: no such user group (or outside the scope).
pub const CONTACT_GROUP_NOT_FOUND: i64 = 42002;
/// authen v2: invalid / used authorization code.
pub const OAUTH_INVALID_GRANT: i64 = 20003;
/// User granted the app but not this scope (lark-cli `errclass/codemeta.go`).
pub const USER_SCOPE_MISSING: i64 = 99991679;
/// refresh_token unknown / malformed.
pub const REFRESH_TOKEN_INVALID: i64 = 20026;
/// refresh_token expired (offline for too long).
pub const REFRESH_TOKEN_EXPIRED: i64 = 20037;
/// refresh_token revoked (user withdrew the grant).
pub const REFRESH_TOKEN_REVOKED: i64 = 20064;
/// refresh_token already used — they are single-use.
pub const REFRESH_TOKEN_REUSED: i64 = 20073;
/// authen v2: invalid client_id / client_secret.
pub const OAUTH_INVALID_CLIENT: i64 = 20002;
/// authen v2: unsupported grant_type.
pub const OAUTH_UNSUPPORTED_GRANT: i64 = 20001;
/// im: sending too often (per user / per chat 5 QPS).
pub const IM_SEND_RATE_LIMITED: i64 = 230020;
/// User identity refused because the app has external sharing / linked organisations enabled.
/// The docs state the restriction but not the code; placeholder until measured in F7.
pub const USER_IDENTITY_SHARED_APP: i64 = 230099;
