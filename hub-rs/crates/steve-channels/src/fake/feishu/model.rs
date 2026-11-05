//! The fake tenant's data: app, bot, chats, members, messages, departments, users.
//!
//! Field names follow the open-platform response bodies so the structs serialize straight
//! into `data`. Ids carry the real prefixes (`oc_`, `ou_`, `on_`, `om_`, `od-`) so adapters that
//! sniff them behave the same as against Feishu.

use serde::Serialize;

/// Default tenant the fake represents; fixtures use the same ids.
pub mod ids {
    pub const APP_ID: &str = "cli_a5fake0000000001";
    pub const APP_SECRET: &str = "fake-app-secret";
    pub const VERIFICATION_TOKEN: &str = "fake-verification-token";
    pub const ENCRYPT_KEY: &str = "fake-encrypt-key";
    pub const TENANT_KEY: &str = "2ca1d211f64f6438";
    pub const EXTERNAL_TENANT_KEY: &str = "736588c9260f175d";
    pub const BOT_NAME: &str = "值班小运";
    pub const BOT_OPEN_ID: &str = "ou_bot0000000000000000000000000001";
    /// Group chat with the bot and three internal members.
    pub const GROUP_CHAT: &str = "oc_group000000000000000000000001";
    /// Group chat with one member from another tenant.
    pub const EXTERNAL_CHAT: &str = "oc_external0000000000000000000001";
    /// Topic-mode group (`chat_mode=topic`).
    pub const TOPIC_CHAT: &str = "oc_topic000000000000000000000001";
    /// The bot's p2p chat with Alice.
    pub const P2P_CHAT: &str = "oc_p2p00000000000000000000000001";
    /// Your own p2p chat with Bob; the bot is not in it (only a user token sees it).
    pub const USER_P2P_CHAT: &str = "oc_userp2p000000000000000000000001";
    /// A group with you and Alice but without the bot.
    pub const NO_BOT_GROUP: &str = "oc_nobot0000000000000000000000001";
    /// "You" (the Steve owner).
    pub const OWNER: User3 = User3 {
        open_id: "ou_owner00000000000000000000000001",
        union_id: "on_owner00000000000000000000000001",
        user_id: "u_owner",
    };
    pub const ALICE: User3 = User3 {
        open_id: "ou_alice00000000000000000000000001",
        union_id: "on_alice00000000000000000000000001",
        user_id: "u_alice",
    };
    pub const BOB: User3 = User3 {
        open_id: "ou_bob0000000000000000000000000001",
        union_id: "on_bob0000000000000000000000000001",
        user_id: "u_bob",
    };
    /// Resigned employee (status.is_resigned = true).
    pub const CAROL: User3 = User3 {
        open_id: "ou_carol00000000000000000000000001",
        union_id: "on_carol00000000000000000000000001",
        user_id: "u_carol",
    };
    /// Member of another tenant, only present in EXTERNAL_CHAT.
    pub const EXTERNAL_USER: User3 = User3 {
        open_id: "ou_ext0000000000000000000000000001",
        union_id: "on_ext0000000000000000000000000001",
        user_id: "",
    };
    pub const DEPT_ENG: &str = "od-eng000000000000000000000000001";
    pub const DEPT_PLATFORM: &str = "od-platform0000000000000000000001";
    pub const DEPT_SALES: &str = "od-sales00000000000000000000000001";

    /// The three ids Feishu gives every user.
    #[derive(Debug, Clone, Copy)]
    pub struct User3 {
        pub open_id: &'static str,
        pub union_id: &'static str,
        pub user_id: &'static str,
    }
}

/// `{union_id, user_id, open_id}` as used in event payloads and mentions.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct UserIds {
    pub union_id: String,
    pub user_id: String,
    pub open_id: String,
}

impl From<ids::User3> for UserIds {
    fn from(u: ids::User3) -> Self {
        Self {
            union_id: u.union_id.into(),
            user_id: u.user_id.into(),
            open_id: u.open_id.into(),
        }
    }
}

impl UserIds {
    /// The id of the requested `*_id_type` (`open_id` when unknown, as Feishu defaults to it).
    pub fn by_type(&self, id_type: &str) -> &str {
        match id_type {
            "union_id" => &self.union_id,
            "user_id" => &self.user_id,
            _ => &self.open_id,
        }
    }
}

/// The app and its bot, as `bot/v3/info` reports it.
#[derive(Debug, Clone)]
pub struct App {
    pub app_id: String,
    pub app_secret: String,
    pub verification_token: String,
    pub encrypt_key: String,
    pub tenant_key: String,
    pub bot_name: String,
    pub bot_open_id: String,
    pub bot_avatar_url: String,
    /// `activate_status`: 2 = activated.
    pub activate_status: i64,
    /// Enterprise self-built app (企业自建应用). Only those may use the long connection.
    pub self_built: bool,
    /// 对外共享 / 关联组织 enabled: user identity can't read message history.
    pub external_sharing: bool,
    /// "刷新 user_access_token" switched on and published; otherwise no refresh_token is issued.
    pub refresh_enabled: bool,
    /// User scopes enabled in the console; a grant is the requested ones ∩ these.
    pub user_scopes: Vec<String>,
}

/// The user scopes Steve asks for.
pub const USER_SCOPES: &[&str] = &[
    "offline_access",
    "im:message",
    "im:message:readonly",
    "im:message.p2p_msg:get_as_user",
    "im:message.group_msg:get_as_user",
    "im:message.send_as_user",
    "im:chat:read",
    "search:message",
];

impl Default for App {
    fn default() -> Self {
        Self {
            app_id: ids::APP_ID.into(),
            app_secret: ids::APP_SECRET.into(),
            verification_token: ids::VERIFICATION_TOKEN.into(),
            encrypt_key: ids::ENCRYPT_KEY.into(),
            tenant_key: ids::TENANT_KEY.into(),
            bot_name: ids::BOT_NAME.into(),
            bot_open_id: ids::BOT_OPEN_ID.into(),
            bot_avatar_url: "https://s1-imfile.feishucdn.com/static-resource/v1/fake-bot-avatar"
                .into(),
            activate_status: 2,
            self_built: true,
            external_sharing: false,
            refresh_enabled: true,
            user_scopes: USER_SCOPES.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// A chat (`im/v1/chats/{chat_id}`). `chat_mode`: `group` | `topic` | `p2p`.
#[derive(Debug, Clone)]
pub struct Chat {
    pub chat_id: String,
    pub name: String,
    pub description: String,
    pub chat_mode: String,
    /// `private` | `public`.
    pub chat_type: String,
    pub owner_open_id: String,
    pub external: bool,
    /// Whether the bot is currently in the chat (only such chats are listed / readable).
    pub bot_in_chat: bool,
    /// `normal` | `dissolved`.
    pub chat_status: String,
}

/// A chat member; `name` and `tenant_key` as `chats/{id}/members` returns them.
#[derive(Debug, Clone)]
pub struct Member {
    pub ids: UserIds,
    pub name: String,
    pub tenant_key: String,
}

/// `mentions[]` entry of a message.
#[derive(Debug, Clone, Serialize)]
pub struct Mention {
    pub key: String,
    pub id: String,
    pub id_type: String,
    pub name: String,
    pub tenant_key: String,
}

/// `sender` of a message in the message API (not the event shape).
#[derive(Debug, Clone, Serialize)]
pub struct Sender {
    pub id: String,
    pub id_type: String,
    /// `user` | `app`.
    pub sender_type: String,
    pub tenant_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Body {
    pub content: String,
}

/// A message as `im/v1/messages` returns it.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub msg_type: String,
    /// Milliseconds since epoch, as a decimal string.
    pub create_time: String,
    pub update_time: String,
    pub deleted: bool,
    pub updated: bool,
    pub chat_id: String,
    pub sender: Sender,
    pub body: Body,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<Mention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upper_message_id: Option<String>,
}

/// A contact-v3 department.
#[derive(Debug, Clone)]
pub struct Department {
    pub open_department_id: String,
    pub department_id: String,
    pub name: String,
    /// Open id of the parent; `"0"` for top-level departments.
    pub parent_department_id: String,
    /// Leader by open id (rendered in the requested `user_id_type`).
    pub leader_open_id: Option<String>,
    pub is_deleted: bool,
    pub order: String,
}

/// A contact-v3 user.
#[derive(Debug, Clone)]
pub struct User {
    pub ids: UserIds,
    pub name: String,
    pub en_name: String,
    pub email: String,
    pub enterprise_email: String,
    pub job_title: String,
    pub leader_open_id: Option<String>,
    /// Open department ids.
    pub department_ids: Vec<String>,
    pub is_resigned: bool,
    pub is_activated: bool,
    pub is_frozen: bool,
}

/// A contact-v3 user group (`contact/v3/group`): only its members matter to the directory sync,
/// as a way users enter the visibility scope.
#[derive(Debug, Clone)]
pub struct UserGroup {
    pub group_id: String,
    pub name: String,
    /// Members by open id.
    pub member_open_ids: Vec<String>,
}

/// A downloadable resource of a message (`messages/{id}/resources/{file_key}`).
#[derive(Debug, Clone)]
pub struct Resource {
    pub message_id: String,
    pub file_key: String,
    pub content_type: String,
    pub file_name: Option<String>,
    pub bytes: Vec<u8>,
}

/// The whole fake tenant. Tests mutate it through [`super::FakeFeishu::world`].
#[derive(Debug, Clone, Default)]
pub struct World {
    pub app: App,
    pub chats: Vec<Chat>,
    /// `(chat_id, member)`.
    pub members: Vec<(String, Member)>,
    pub messages: Vec<Message>,
    pub resources: Vec<Resource>,
    pub departments: Vec<Department>,
    pub users: Vec<User>,
    /// Contact visibility (`contact/v3/scopes`): open department ids and open user ids.
    pub scope_department_ids: Vec<String>,
    pub scope_user_ids: Vec<String>,
    /// User groups whose members are inside the visibility scope.
    pub scope_group_ids: Vec<String>,
    pub groups: Vec<UserGroup>,
    /// The user who completes the fake OAuth flow (F6).
    pub oauth_user_open_id: String,
    /// Open question (b): whether a user token can read a group the bot
    /// isn't in. `false` (default) lets it; `true` answers "bot not in chat".
    pub user_read_requires_bot: bool,
    /// Whether user-identity search finds messages of groups without the bot (also open).
    pub search_sees_no_bot_groups: bool,
    /// The tenant's name (`tenant/v2/tenant/query`).
    pub tenant_name: String,
}

impl World {
    /// Empty tenant with just the app.
    pub fn empty() -> Self {
        Self {
            scope_department_ids: vec!["0".into()],
            oauth_user_open_id: ids::OWNER.open_id.into(),
            search_sees_no_bot_groups: true,
            ..Self::default()
        }
    }

    /// A small tenant matching the fixtures: owner, Alice, Bob, resigned Carol, an external user,
    /// three departments, a group, an external group, a topic group and a p2p chat with Alice.
    pub fn sample() -> Self {
        let mut w = Self::empty();
        w.tenant_name = "示例科技".into();
        let tk = w.app.tenant_key.clone();
        let user = |u: ids::User3, name: &str, en: &str, title: &str, depts: &[&str]| User {
            ids: u.into(),
            name: name.into(),
            en_name: en.into(),
            email: format!("{}@example.com", en.to_lowercase()),
            enterprise_email: format!("{}@corp.example.com", en.to_lowercase()),
            job_title: title.into(),
            leader_open_id: None,
            department_ids: depts.iter().map(|d| d.to_string()).collect(),
            is_resigned: false,
            is_activated: true,
            is_frozen: false,
        };
        w.users = vec![
            user(ids::OWNER, "李鹏翔", "Owner", "负责人", &[ids::DEPT_ENG]),
            User {
                leader_open_id: Some(ids::OWNER.open_id.into()),
                ..user(
                    ids::ALICE,
                    "王小丽",
                    "Alice",
                    "后端工程师",
                    &[ids::DEPT_PLATFORM],
                )
            },
            User {
                leader_open_id: Some(ids::OWNER.open_id.into()),
                ..user(ids::BOB, "张伟", "Bob", "销售", &[ids::DEPT_SALES])
            },
            User {
                is_resigned: true,
                is_activated: false,
                ..user(
                    ids::CAROL,
                    "陈静",
                    "Carol",
                    "前端工程师",
                    &[ids::DEPT_PLATFORM],
                )
            },
        ];
        let dept =
            |id: &str, num: &str, name: &str, parent: &str, leader: Option<&str>| Department {
                open_department_id: id.into(),
                department_id: num.into(),
                name: name.into(),
                parent_department_id: parent.into(),
                leader_open_id: leader.map(str::to_string),
                is_deleted: false,
                order: "0".into(),
            };
        w.departments = vec![
            dept(
                ids::DEPT_ENG,
                "D001",
                "研发部",
                "0",
                Some(ids::OWNER.open_id),
            ),
            dept(
                ids::DEPT_PLATFORM,
                "D002",
                "平台组",
                ids::DEPT_ENG,
                Some(ids::ALICE.open_id),
            ),
            dept(
                ids::DEPT_SALES,
                "D003",
                "销售部",
                "0",
                Some(ids::BOB.open_id),
            ),
        ];
        let chat = |id: &str, name: &str, mode: &str, external: bool| Chat {
            chat_id: id.into(),
            name: name.into(),
            description: String::new(),
            chat_mode: mode.into(),
            chat_type: "private".into(),
            owner_open_id: ids::OWNER.open_id.into(),
            external,
            bot_in_chat: true,
            chat_status: "normal".into(),
        };
        w.chats = vec![
            chat(ids::GROUP_CHAT, "平台值班群", "group", false),
            chat(ids::EXTERNAL_CHAT, "客户对接群", "group", true),
            chat(ids::TOPIC_CHAT, "需求讨论", "topic", false),
            Chat {
                name: String::new(),
                owner_open_id: String::new(),
                ..chat(ids::P2P_CHAT, "", "p2p", false)
            },
            Chat {
                name: String::new(),
                owner_open_id: String::new(),
                bot_in_chat: false,
                ..chat(ids::USER_P2P_CHAT, "", "p2p", false)
            },
            Chat {
                bot_in_chat: false,
                ..chat(ids::NO_BOT_GROUP, "读书会", "group", false)
            },
        ];
        let member = |u: ids::User3, name: &str, tk: &str| Member {
            ids: u.into(),
            name: name.into(),
            tenant_key: tk.into(),
        };
        for c in [ids::GROUP_CHAT, ids::EXTERNAL_CHAT, ids::TOPIC_CHAT] {
            w.members
                .push((c.into(), member(ids::OWNER, "李鹏翔", &tk)));
            w.members
                .push((c.into(), member(ids::ALICE, "王小丽", &tk)));
        }
        w.members
            .push((ids::GROUP_CHAT.into(), member(ids::BOB, "张伟", &tk)));
        w.members.push((
            ids::EXTERNAL_CHAT.into(),
            member(ids::EXTERNAL_USER, "赵客户", ids::EXTERNAL_TENANT_KEY),
        ));
        w.members
            .push((ids::P2P_CHAT.into(), member(ids::ALICE, "王小丽", &tk)));
        for (c, u, n) in [
            (ids::USER_P2P_CHAT, ids::OWNER, "李鹏翔"),
            (ids::USER_P2P_CHAT, ids::BOB, "张伟"),
            (ids::NO_BOT_GROUP, ids::OWNER, "李鹏翔"),
            (ids::NO_BOT_GROUP, ids::ALICE, "王小丽"),
        ] {
            w.members.push((c.into(), member(u, n, &tk)));
        }

        // History the fixtures refer to: the root of the topic thread, and an earlier bot message.
        let user_sender = |u: ids::User3| Sender {
            id: u.open_id.into(),
            id_type: "open_id".into(),
            sender_type: "user".into(),
            tenant_key: tk.clone(),
        };
        let text = |t: &str| Body {
            content: serde_json::json!({"text": t}).to_string(),
        };
        w.messages.push(Message {
            message_id: "om_topicroot000000000000000000001".into(),
            root_id: None,
            parent_id: None,
            thread_id: Some("omt_fake0000000001".into()),
            msg_type: "text".into(),
            create_time: "1761899000000".into(),
            update_time: "1761899000000".into(),
            deleted: false,
            updated: false,
            chat_id: ids::TOPIC_CHAT.into(),
            sender: user_sender(ids::ALICE),
            body: text("需求：导出报表支持按部门筛选"),
            mentions: Vec::new(),
            upper_message_id: None,
        });
        w.messages.push(Message {
            message_id: "om_botearlier00000000000000000001".into(),
            root_id: None,
            parent_id: None,
            thread_id: None,
            msg_type: "text".into(),
            create_time: "1761899500000".into(),
            update_time: "1761899500000".into(),
            deleted: false,
            updated: false,
            chat_id: ids::GROUP_CHAT.into(),
            sender: Sender {
                id: w.app.app_id.clone(),
                id_type: "app_id".into(),
                sender_type: "app".into(),
                tenant_key: tk.clone(),
            },
            body: text("昨晚 23:10 的告警已恢复。"),
            mentions: Vec::new(),
            upper_message_id: None,
        });
        // What only your user token can read: Bob's DM to you, a group without the bot.
        let plain = |id: &str, chat: &str, from: ids::User3, t: &str, at: &str| Message {
            message_id: id.into(),
            root_id: None,
            parent_id: None,
            thread_id: None,
            msg_type: "text".into(),
            create_time: at.into(),
            update_time: at.into(),
            deleted: false,
            updated: false,
            chat_id: chat.into(),
            sender: user_sender(from),
            body: text(t),
            mentions: Vec::new(),
            upper_message_id: None,
        };
        w.messages.push(plain(
            "om_userp2p0000000000000000000000001",
            ids::USER_P2P_CHAT,
            ids::BOB,
            "在吗？客户那边的合同明天要签",
            "1761899600000",
        ));
        w.messages.push(plain(
            "om_nobot00000000000000000000000001",
            ids::NO_BOT_GROUP,
            ids::ALICE,
            "这周读第三章",
            "1761899300000",
        ));
        // Attachments of the image / file fixtures.
        w.resources.push(Resource {
            message_id: "om_image00000000000000000000000001".into(),
            file_key: "img_v3_fake0000-0000-0000-0000-000000000001g".into(),
            content_type: "image/png".into(),
            file_name: None,
            bytes: b"\x89PNG\r\n\x1a\nfake-image".to_vec(),
        });
        w.resources.push(Resource {
            message_id: "om_file000000000000000000000000001".into(),
            file_key: "file_v3_fake0000-0000-0000-0000-000000000001g".into(),
            content_type: "text/plain".into(),
            file_name: Some("告警截图说明.txt".into()),
            bytes: "告警在 23:10 出现，23:25 恢复。".as_bytes().to_vec(),
        });
        w
    }

    pub fn chat(&self, chat_id: &str) -> Option<&Chat> {
        self.chats.iter().find(|c| c.chat_id == chat_id)
    }

    pub fn chat_members(&self, chat_id: &str) -> impl Iterator<Item = &Member> {
        self.members
            .iter()
            .filter(move |(c, _)| c == chat_id)
            .map(|(_, m)| m)
    }

    pub fn user_by_open_id(&self, open_id: &str) -> Option<&User> {
        self.users.iter().find(|u| u.ids.open_id == open_id)
    }

    /// Finds a user by any of their ids.
    pub fn user_by_any_id(&self, id: &str) -> Option<&User> {
        self.users
            .iter()
            .find(|u| u.ids.open_id == id || u.ids.union_id == id || u.ids.user_id == id)
    }

    pub fn department(&self, open_id: &str) -> Option<&Department> {
        self.departments
            .iter()
            .find(|d| d.open_department_id == open_id || d.department_id == open_id)
    }

    /// Whether a user is inside the contact visibility scope: listed, in a listed group, or in a
    /// visible department (resigned users stay readable by id, as in Feishu).
    pub fn user_visible(&self, u: &User) -> bool {
        let open = &u.ids.open_id;
        self.scope_department_ids.iter().any(|s| s == "0")
            || self.scope_user_ids.contains(open)
            || self.groups.iter().any(|g| {
                self.scope_group_ids.contains(&g.group_id) && g.member_open_ids.contains(open)
            })
            || u.department_ids.iter().any(|d| self.department_visible(d))
    }

    /// Whether a department is inside the contact visibility scope (itself or an ancestor listed).
    pub fn department_visible(&self, open_id: &str) -> bool {
        if self.scope_department_ids.iter().any(|s| s == "0") {
            return true;
        }
        let mut cur = Some(open_id.to_string());
        let mut guard = 0;
        while let Some(id) = cur {
            if self.scope_department_ids.contains(&id) {
                return true;
            }
            guard += 1;
            if guard > 64 {
                return false;
            }
            cur = self
                .department(&id)
                .map(|d| d.parent_department_id.clone())
                .filter(|p| p != "0");
        }
        false
    }
}
