//! Shared digest assertions over backend-owned source/transaction fixtures.
//! Mem uses verify_* rows; SQLite supplies real comms tables through the Harness hook.
use super::{Harness, NamedCheck, block_on, entities::cell, mem::Data};
use crate::{entity::*, ops::comms::digest::*, query::*, registry::EntryKind, uow::*, *};
use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    sync::Arc,
};

const TEXT: StaticFieldType = StaticFieldType::Text;
const INT: StaticFieldType = StaticFieldType::Int;
macro_rules! fields {($($name:literal : $ty:expr),* $(,)?)=>{&[$(Field{name:$name,ty:$ty,nullable:false}),*]};}
crate::entity_desc!(pub SOURCE={name:"verify_digest_source",domain:Domain::Comms,key:&["source","id"],fields:fields!["source":TEXT,"id":TEXT,"workspace_id":TEXT,"conv":TEXT,"status":TEXT,"at":INT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
crate::entity_desc!(pub RECEIPT={name:"verify_digest_receipt",domain:Domain::Comms,key:&["workspace_id","conv","message_id"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"message_id":TEXT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
crate::entity_desc!(pub PENDING={name:"verify_digest_pending",domain:Domain::Comms,key:&["workspace_id","conv","message_id"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"message_id":TEXT,"at":INT,"position":INT],indexes:&[],kind:StaticEntityKind::Log,retention:None});
crate::entity_desc!(pub CONVS={name:"verify_digest_convs",domain:Domain::Comms,key:&["workspace_id","conv"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"messages":INT,"oldest":INT,"newest":INT,"marker":INT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
pub fn descriptors() -> Vec<EntityDesc> {
    vec![
        SOURCE.to_wire(),
        RECEIPT.to_wire(),
        PENDING.to_wire(),
        CONVS.to_wire(),
    ]
}
fn text(row: &Row, field: &str) -> String {
    match cell(row, field) {
        Some(Cell::Text(value)) => value.clone(),
        _ => String::new(),
    }
}
fn int(row: &Row, field: &str) -> i64 {
    match cell(row, field) {
        Some(Cell::Int(value)) => *value,
        _ => 0,
    }
}
fn identity(row: &Row) -> (String, String, String) {
    (
        text(row, "workspace_id"),
        text(row, "conv"),
        text(row, "message_id"),
    )
}
fn record(fields: &[(&str, Cell)]) -> Row {
    Row(fields
        .iter()
        .map(|(k, v)| ((*k).into(), v.clone()))
        .collect())
}
fn string(value: &str) -> Cell {
    Cell::Text(value.into())
}
pub(super) fn apply(
    data: &mut Data,
    entities: &BTreeMap<String, EntityDesc>,
    op: &AnyOp,
    rev: Rev,
) -> Result<(), Error> {
    if op.name != SYNC_PENDING_MEMBERSHIP.name {
        return Err(error(
            steve_types::Code::Unsupported,
            "named operation not implemented",
        ));
    }
    if !descriptors()
        .iter()
        .all(|expected| entities.get(&expected.name) == Some(expected))
    {
        return Err(error(
            steve_types::Code::Unsupported,
            "digest fixture entities not registered",
        ));
    }
    let request: SyncPendingMembership = op.decode()?;
    request.validate()?;
    if request.source == Source::Receipt {
        let (conv, id) = request.receipt_identity()?;
        let key = (request.workspace_id.as_str().to_owned(), conv, id);
        if !data
            .rows
            .get(RECEIPT.name)
            .into_iter()
            .flatten()
            .any(|(row, _)| identity(row) == key)
        {
            return Err(error(
                steve_types::Code::Invalid,
                "receipt must be inserted in the same work unit",
            ));
        }
        if request.entry == EntryPoint::DequeueOnReceipt {
            data.rows
                .entry(PENDING.name.into())
                .or_default()
                .retain(|(row, _)| identity(row) != key);
        }
    } else {
        let (source, prefix) = match request.source {
            Source::ChanMessage => ("chan_message", "space:"),
            Source::MailMessage => ("mail_message", "mail_thread:"),
            Source::Receipt => {
                return Err(error(
                    steve_types::Code::Invalid,
                    "receipt is not a message",
                ));
            }
        };
        let source = data
            .rows
            .get(SOURCE.name)
            .into_iter()
            .flatten()
            .find(|(row, _)| text(row, "source") == source && text(row, "id") == request.id)
            .map(|(row, _)| row.clone());
        let old_workspaces: Vec<_> = data
            .rows
            .get(PENDING.name)
            .into_iter()
            .flatten()
            .filter(|(row, _)| {
                text(row, "message_id") == request.id && text(row, "conv").starts_with(prefix)
            })
            .map(|(row, _)| text(row, "workspace_id"))
            .collect();
        if source
            .as_ref()
            .is_some_and(|row| text(row, "workspace_id") != request.workspace_id.as_str())
            || (source.is_none()
                && old_workspaces
                    .iter()
                    .any(|ws| ws != request.workspace_id.as_str()))
        {
            return Err(error(
                steve_types::Code::Invalid,
                "digest source workspace mismatch",
            ));
        }
        if request.entry == EntryPoint::EnqueueDigestible {
            let desired = source.filter(|row| {
                text(row, "workspace_id") == request.workspace_id.as_str()
                    && text(row, "conv").starts_with(prefix)
                    && ["received", "sent"].contains(&text(row, "status").as_str())
            });
            let desired = desired.filter(|row| {
                !data
                    .rows
                    .get(RECEIPT.name)
                    .into_iter()
                    .flatten()
                    .any(|(receipt, _)| {
                        identity(receipt)
                            == (
                                text(row, "workspace_id"),
                                text(row, "conv"),
                                request.id.clone(),
                            )
                    })
            });
            let pending = data.rows.entry(PENDING.name.into()).or_default();
            pending.retain(|(row, _)| {
                text(row, "message_id") != request.id
                    || !text(row, "conv").starts_with(prefix)
                    || desired.as_ref().is_some_and(|source| {
                        identity(row)
                            == (
                                text(source, "workspace_id"),
                                text(source, "conv"),
                                request.id.clone(),
                            )
                    })
            });
            if let Some(source) = desired {
                let key = (
                    text(&source, "workspace_id"),
                    text(&source, "conv"),
                    request.id.clone(),
                );
                if let Some((row, old_rev)) =
                    pending.iter_mut().find(|(row, _)| identity(row) == key)
                {
                    super::entities::set(row, "at", Cell::Int(int(&source, "at")));
                    *old_rev = rev;
                } else {
                    let next = data
                        .seqs
                        .entry((Domain::Comms, PENDING.name.into()))
                        .or_default();
                    *next = next
                        .checked_add(1)
                        .filter(|v| *v <= i64::MAX as u64)
                        .ok_or_else(|| {
                            error(steve_types::Code::Exhausted, "digest position exhausted")
                        })?;
                    pending.push((
                        record(&[
                            ("workspace_id", string(&key.0)),
                            ("conv", string(&key.1)),
                            ("message_id", string(&key.2)),
                            ("at", Cell::Int(int(&source, "at"))),
                            ("position", Cell::Int(*next as i64)),
                        ]),
                        rev,
                    ));
                }
            }
        }
    }
    let mut groups: BTreeMap<(String, String), (i64, i64, i64)> = BTreeMap::new();
    for (row, _) in data.rows.get(PENDING.name).into_iter().flatten() {
        let at = int(row, "at");
        let group = groups
            .entry((text(row, "workspace_id"), text(row, "conv")))
            .or_insert((0, at, at));
        group.0 += 1;
        group.1 = group.1.min(at);
        group.2 = group.2.max(at);
    }
    let rows = groups
        .into_iter()
        .map(|((workspace, conv), (messages, oldest, newest))| {
            (
                record(&[
                    ("workspace_id", string(&workspace)),
                    ("conv", string(&conv)),
                    ("messages", Cell::Int(messages)),
                    ("oldest", Cell::Int(oldest)),
                    ("newest", Cell::Int(newest)),
                    (
                        "marker",
                        Cell::Int(
                            data.rows
                                .get(CONVS.name)
                                .into_iter()
                                .flatten()
                                .find(|(row, _)| {
                                    text(row, "workspace_id") == workspace
                                        && text(row, "conv") == conv
                                })
                                .map_or(0, |(row, _)| int(row, "marker")),
                        ),
                    ),
                ]),
                rev,
            )
        })
        .collect();
    data.rows.insert(CONVS.name.into(), rows);
    Ok(())
}

/// Test-only fixture actions. Backends arrange source mutations and named operations in
/// one transaction, without pretending real message tables implement generic StoredEntity.
#[derive(Clone, Debug)]
pub enum Change {
    Write {
        row: Row,
        insert_only: bool,
    },
    Delete {
        source: Source,
        id: String,
    },
    Receipt {
        workspace: String,
        conv: String,
        id: String,
    },
    Marker {
        workspace: String,
        conv: String,
        value: i64,
    },
    SkewProjection {
        workspace: String,
        conv: String,
    },
    Op(SyncPendingMembership),
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub sources: Vec<Row>,
    pub receipts: Vec<Row>,
    pub pending: Vec<Row>,
    pub conversations: Vec<Row>,
}
pub trait Fixture: Send + Sync {
    fn transact(&self, h: &dyn Harness, id: &str, changes: Vec<Change>) -> Result<(), CommitError>;
    fn state(&self) -> Result<State, Error>;
    /// Every scenario endpoint audits the real backend; SQLite calls verify_pending.
    fn audit(&self) -> Result<(), String>;
}
struct MemoryFixture(Arc<dyn DomainStore>);
pub(super) fn memory_fixture(store: Arc<dyn DomainStore>) -> Box<dyn Fixture> {
    Box::new(MemoryFixture(store))
}
fn rows(store: &dyn DomainStore, entity: &str) -> Result<Vec<Row>, Error> {
    let query = Scan::<Row> {
        index: IndexName {
            entity: entity.into(),
            name: String::new(),
        },
        range: KeyRange::default(),
        filter: vec![],
        order: Order::Asc,
        limit: 500,
        budget: ScanBudget(5000),
        resume: None,
        marker: PhantomData,
    };
    let output = block_on(store.query(AnyQuery {
        name: "scan".into(),
        domain: Domain::Comms,
        body: Payload::Json(encode(&query)?),
    }))?;
    let page: Page<Row> = output.body.decode()?;
    Ok(page.rows.into_iter().map(|(row, _)| row).collect())
}
impl Fixture for MemoryFixture {
    fn transact(&self, h: &dyn Harness, id: &str, changes: Vec<Change>) -> Result<(), CommitError> {
        let mutations = changes
            .into_iter()
            .map(|change| {
                Ok(match change {
                    Change::Write {
                        row,
                        insert_only: true,
                    } => Mutation::Insert {
                        entity: SOURCE.name.into(),
                        row,
                    },
                    Change::Write {
                        row,
                        insert_only: false,
                    } => Mutation::Upsert {
                        entity: SOURCE.name.into(),
                        row,
                    },
                    Change::Delete { source, id } => Mutation::Delete {
                        entity: SOURCE.name.into(),
                        key: vec![string(source_name(source)), string(&id)],
                    },
                    Change::Receipt {
                        workspace,
                        conv,
                        id,
                    } => Mutation::Insert {
                        entity: RECEIPT.name.into(),
                        row: record(&[
                            ("workspace_id", string(&workspace)),
                            ("conv", string(&conv)),
                            ("message_id", string(&id)),
                        ]),
                    },
                    Change::Marker {
                        workspace,
                        conv,
                        value,
                    } => Mutation::Update {
                        entity: CONVS.name.into(),
                        key: vec![string(&workspace), string(&conv)],
                        set: record(&[("marker", Cell::Int(value))]),
                    },
                    Change::SkewProjection { workspace, conv } => Mutation::Update {
                        entity: CONVS.name.into(),
                        key: vec![string(&workspace), string(&conv)],
                        set: record(&[
                            ("messages", Cell::Int(-1)),
                            ("oldest", Cell::Int(-1)),
                            ("newest", Cell::Int(-1)),
                        ]),
                    },
                    Change::Op(op) => Mutation::Op(op.to_wire().map_err(CommitError::Fatal)?),
                })
            })
            .collect::<Result<Vec<_>, CommitError>>()?;
        block_on(
            self.0
                .commit(super::suite::uow(h, Domain::Comms, id, mutations)),
        )
        .map(|_| ())
    }
    fn state(&self) -> Result<State, Error> {
        Ok(State {
            sources: rows(self.0.as_ref(), SOURCE.name)?,
            receipts: rows(self.0.as_ref(), RECEIPT.name)?,
            pending: rows(self.0.as_ref(), PENDING.name)?,
            conversations: rows(self.0.as_ref(), CONVS.name)?,
        })
    }
    fn audit(&self) -> Result<(), String> {
        let state = self.state().map_err(|e| e.to_string())?;
        let receipts: BTreeSet<_> = state.receipts.iter().map(identity).collect();
        let expected: BTreeMap<_, _> = state
            .sources
            .iter()
            .filter(|r| ["received", "sent"].contains(&text(r, "status").as_str()))
            .map(|r| {
                (
                    (text(r, "workspace_id"), text(r, "conv"), text(r, "id")),
                    int(r, "at"),
                )
            })
            .filter(|(k, _)| !receipts.contains(k))
            .collect();
        let actual: BTreeMap<_, _> = state
            .pending
            .iter()
            .map(|r| (identity(r), int(r, "at")))
            .collect();
        if actual != expected {
            return Err("memory digest membership mismatch".into());
        }
        let mut groups: BTreeMap<_, Vec<i64>> = BTreeMap::new();
        for ((ws, conv, _), at) in actual {
            groups.entry((ws, conv)).or_default().push(at);
        }
        if groups.len() != state.conversations.len() {
            return Err("memory digest conversation mismatch".into());
        }
        for row in state.conversations {
            let ats = groups
                .get(&(text(&row, "workspace_id"), text(&row, "conv")))
                .ok_or("unexpected conversation")?;
            if (
                int(&row, "messages"),
                int(&row, "oldest"),
                int(&row, "newest"),
            ) != (
                ats.len() as i64,
                *ats.iter().min().unwrap(),
                *ats.iter().max().unwrap(),
            ) {
                return Err("memory digest projection mismatch".into());
            }
        }
        Ok(())
    }
}
pub fn source_name(source: Source) -> &'static str {
    match source {
        Source::ChanMessage => "chan_message",
        Source::MailMessage => "mail_message",
        Source::Receipt => "receipt",
    }
}
pub fn message(
    id: &str,
    source: Source,
    workspace: &str,
    conv: &str,
    status: &str,
    at: i64,
) -> Row {
    record(&[
        ("source", string(source_name(source))),
        ("id", string(id)),
        ("workspace_id", string(workspace)),
        ("conv", string(conv)),
        ("status", string(status)),
        ("at", Cell::Int(at)),
    ])
}
fn write(id: &str, source: Source, workspace: &str, conv: &str, status: &str, at: i64) -> Change {
    Change::Write {
        row: message(id, source, workspace, conv, status, at),
        insert_only: false,
    }
}
fn command(id: &str, source: Source, workspace: &str) -> Change {
    Change::Op(
        SyncPendingMembership::enqueue_digestible(source, id.into(), workspace.into()).unwrap(),
    )
}
fn transact(
    f: &dyn Fixture,
    h: &dyn Harness,
    id: &str,
    changes: Vec<Change>,
) -> Result<(), CommitError> {
    let result = f.transact(h, id, changes);
    f.audit().unwrap();
    result
}
fn pending<'a>(s: &'a State, id: &str) -> &'a Row {
    s.pending
        .iter()
        .find(|r| text(r, "message_id") == id)
        .unwrap()
}
fn conversation<'a>(s: &'a State, ws: &str, conv: &str) -> &'a Row {
    s.conversations
        .iter()
        .find(|r| text(r, "workspace_id") == ws && text(r, "conv") == conv)
        .unwrap()
}
fn rejected(result: Result<(), CommitError>) {
    assert!(
        matches!(result, Err(CommitError::Fatal(e)) if e.code == steve_types::Code::Invalid),
        "invalid scope or missing receipt must produce the contract's Invalid error"
    );
}
/// The sole assertion set for Mem, Loopback<Mem> and both SQLite physical layouts.
pub fn check(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let f = h.digest_fixture(store).unwrap();
    let f = f.as_ref();
    let source = Source::ChanMessage;
    for (id, at) in [("a", 30), ("b", 10), ("c", 20)] {
        transact(
            f,
            h,
            id,
            vec![
                write(id, source, "ws", "space:old", "received", at),
                command(id, source, "ws"),
            ],
        )
        .unwrap();
    }
    transact(
        f,
        h,
        "marker",
        vec![Change::Marker {
            workspace: "ws".into(),
            conv: "space:old".into(),
            value: 73,
        }],
    )
    .unwrap();
    let first = f.state().unwrap();
    let conv = conversation(&first, "ws", "space:old");
    assert_eq!(
        (
            int(conv, "messages"),
            int(conv, "oldest"),
            int(conv, "newest")
        ),
        (3, 10, 30)
    );
    transact(f, h, "repeat", vec![command("a", source, "ws")]).unwrap();
    assert_eq!(f.state().unwrap(), first);
    transact(
        f,
        h,
        "timestamp",
        vec![
            write("a", source, "ws", "space:old", "received", 35),
            command("a", source, "ws"),
        ],
    )
    .unwrap();
    let changed = f.state().unwrap();
    assert_eq!(
        int(pending(&changed, "a"), "position"),
        int(pending(&first, "a"), "position")
    );
    assert_eq!(int(conversation(&changed, "ws", "space:old"), "marker"), 73);
    assert_eq!(int(conversation(&changed, "ws", "space:old"), "newest"), 35);
    let ack = SyncPendingMembership::dequeue_on_receipt("space:old", "b", "ws".into()).unwrap();
    rejected(transact(
        f,
        h,
        "missing-receipt",
        vec![Change::Op(ack.clone())],
    ));
    assert_eq!(f.state().unwrap(), changed);
    for (n, ws, conv) in [
        ("wrong-conv", "ws", "space:else"),
        ("wrong-ws", "other", "space:old"),
    ] {
        transact(
            f,
            h,
            n,
            vec![
                Change::Receipt {
                    workspace: ws.into(),
                    conv: conv.into(),
                    id: "b".into(),
                },
                Change::Op(
                    SyncPendingMembership::dequeue_on_receipt(conv, "b", ws.into()).unwrap(),
                ),
            ],
        )
        .unwrap();
        assert_eq!(f.state().unwrap().pending, changed.pending);
    }
    transact(
        f,
        h,
        "ack",
        vec![
            Change::Receipt {
                workspace: "ws".into(),
                conv: "space:old".into(),
                id: "b".into(),
            },
            Change::Op(ack),
        ],
    )
    .unwrap();
    transact(f, h, "no-requeue", vec![command("b", source, "ws")]).unwrap();
    let acknowledged = f.state().unwrap();
    assert_eq!(acknowledged.pending.len(), 2);
    assert_eq!(
        int(conversation(&acknowledged, "ws", "space:old"), "marker"),
        73
    );
    transact(
        f,
        h,
        "move",
        vec![
            write("a", source, "other", "space:new", "sent", 5),
            command("a", source, "other"),
        ],
    )
    .unwrap();
    let moved = f.state().unwrap();
    assert_eq!(moved.pending.len(), 2);
    assert_eq!(text(pending(&moved, "a"), "workspace_id"), "other");
    assert!(
        int(pending(&moved, "a"), "position")
            > first
                .pending
                .iter()
                .map(|r| int(r, "position"))
                .max()
                .unwrap()
    );
    transact(
        f,
        h,
        "recall",
        vec![
            write("c", source, "ws", "space:old", "recalled", 20),
            command("c", source, "ws"),
        ],
    )
    .unwrap();
    assert_eq!(f.state().unwrap().conversations.len(), 1);
    transact(
        f,
        h,
        "delete",
        vec![
            Change::Delete {
                source,
                id: "a".into(),
            },
            command("a", source, "other"),
        ],
    )
    .unwrap();
    let empty = f.state().unwrap();
    assert!(empty.pending.is_empty() && empty.conversations.is_empty());
    let source = Source::MailMessage;
    transact(
        f,
        h,
        "mail",
        vec![
            write("mail", source, "ws", "mail_thread:t", "queued", 40),
            command("mail", source, "ws"),
        ],
    )
    .unwrap();
    assert!(f.state().unwrap().pending.is_empty());
    transact(
        f,
        h,
        "mail-sent",
        vec![
            write("mail", source, "ws", "mail_thread:t", "sent", 40),
            command("mail", source, "ws"),
        ],
    )
    .unwrap();
    let mail = f.state().unwrap();
    assert_eq!(mail.pending.len(), 1);
    assert!(int(&mail.pending[0], "position") > int(pending(&moved, "a"), "position"));
    transact(
        f,
        h,
        "mail-marker",
        vec![Change::Marker {
            workspace: "ws".into(),
            conv: "mail_thread:t".into(),
            value: 91,
        }],
    )
    .unwrap();
    let mail = f.state().unwrap();
    let reconcile =
        SyncPendingMembership::sync_pending_conversation(source, "mail".into(), "ws".into());
    transact(
        f,
        h,
        "reconcile",
        vec![
            Change::SkewProjection {
                workspace: "ws".into(),
                conv: "mail_thread:t".into(),
            },
            Change::Op(reconcile.clone()),
        ],
    )
    .unwrap();
    assert_eq!(f.state().unwrap(), mail);
    for (id, entry) in [
        ("bad-ws-enqueue", EntryPoint::EnqueueDigestible),
        ("bad-ws-sync", EntryPoint::SyncPendingConversation),
    ] {
        let mut bad = reconcile.clone();
        bad.workspace_id = "wrong".into();
        bad.entry = entry;
        rejected(transact(f, h, id, vec![Change::Op(bad)]));
        assert_eq!(f.state().unwrap(), mail);
    }
    rejected(transact(
        f,
        h,
        "bad-ws-delete",
        vec![
            Change::Delete {
                source,
                id: "mail".into(),
            },
            command("mail", source, "wrong"),
        ],
    ));
    assert_eq!(f.state().unwrap(), mail);
    // A channel and mail message may have the same id: old scope recovery is source-specific.
    transact(
        f,
        h,
        "collision",
        vec![
            write(
                "mail",
                Source::ChanMessage,
                "ws",
                "space:collision",
                "received",
                8,
            ),
            command("mail", Source::ChanMessage, "ws"),
        ],
    )
    .unwrap();
    assert_eq!(f.state().unwrap().pending.len(), 2);
    transact(
        f,
        h,
        "delete-collision",
        vec![
            Change::Delete {
                source: Source::ChanMessage,
                id: "mail".into(),
            },
            command("mail", Source::ChanMessage, "ws"),
        ],
    )
    .unwrap();
    assert_eq!(f.state().unwrap(), mail);
    let failure = transact(
        f,
        h,
        "digest-atomic",
        vec![
            write("new", source, "ws", "mail_thread:t", "received", 50),
            command("new", source, "ws"),
            Change::Write {
                row: message("mail", source, "ws", "mail_thread:t", "sent", 40),
                insert_only: true,
            },
        ],
    );
    assert!(failure.is_err());
    assert_eq!(
        f.state().unwrap(),
        mail,
        "late failure rolls source, membership and metadata back together"
    );
    let mut receipt_sync =
        SyncPendingMembership::dequeue_on_receipt("space:old", "b", "ws".into()).unwrap();
    receipt_sync.entry = EntryPoint::SyncPendingConversation;
    transact(f, h, "receipt-reconcile", vec![Change::Op(receipt_sync)]).unwrap();
    assert_eq!(
        f.state().unwrap(),
        mail,
        "receipt reconciliation never re-enqueues acknowledged messages"
    );
}
/// The registry still names one semantic check; backends supply fixtures, not assertions.
pub fn named_checks() -> Vec<NamedCheck> {
    vec![NamedCheck {
        name: SYNC_PENDING_MEMBERSHIP.name,
        domain: Domain::Comms,
        kind: EntryKind::Operation,
        check,
    }]
}
