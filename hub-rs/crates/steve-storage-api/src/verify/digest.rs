//! Digest semantic fixture for the first named operation. These verify_* rows model source
//! changes without depending on production schemas. The named check is enabled by the
//! catalogue's implemented flag; MemStore tests exercise it even before that flag changes.
use super::{Harness, NamedCheck, block_on, entities::cell, mem::Data};
use crate::{entity::*, ops::comms::digest::*, query::*, registry::EntryKind, uow::*, *};
use std::{collections::BTreeMap, marker::PhantomData, sync::Arc};

const TEXT: StaticFieldType = StaticFieldType::Text;
const INT: StaticFieldType = StaticFieldType::Int;
macro_rules! fields {($($name:literal : $ty:expr),* $(,)?)=>{&[$(Field{name:$name,ty:$ty,nullable:false}),*]};}
crate::entity_desc!(pub SOURCE={name:"verify_digest_source",domain:Domain::Comms,key:&["source","id"],fields:fields!["source":TEXT,"id":TEXT,"workspace_id":TEXT,"conv":TEXT,"status":TEXT,"at":INT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
crate::entity_desc!(pub RECEIPT={name:"verify_digest_receipt",domain:Domain::Comms,key:&["workspace_id","conv","message_id"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"message_id":TEXT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
crate::entity_desc!(pub PENDING={name:"verify_digest_pending",domain:Domain::Comms,key:&["workspace_id","conv","message_id"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"message_id":TEXT,"at":INT,"position":INT],indexes:&[],kind:StaticEntityKind::Log,retention:None});
crate::entity_desc!(pub CONVS={name:"verify_digest_convs",domain:Domain::Comms,key:&["workspace_id","conv"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"messages":INT,"oldest":INT,"newest":INT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
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
    if request.entry == EntryPoint::DequeueOnReceipt {
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
        data.rows
            .entry(PENDING.name.into())
            .or_default()
            .retain(|(row, _)| identity(row) != key);
    } else if request.entry == EntryPoint::EnqueueDigestible {
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
                        && int(row, "at") == int(source, "at")
                })
        });
        if let Some(source) = desired {
            let key = (
                text(&source, "workspace_id"),
                text(&source, "conv"),
                request.id.clone(),
            );
            if !pending.iter().any(|(row, _)| identity(row) == key) {
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
                ]),
                rev,
            )
        })
        .collect();
    data.rows.insert(CONVS.name.into(), rows);
    Ok(())
}
/// Registered semantic test; presently opt-in because the public catalogue is declaration-only.
pub fn named_checks() -> Vec<NamedCheck> {
    vec![NamedCheck {
        name: SYNC_PENDING_MEMBERSHIP.name,
        domain: Domain::Comms,
        kind: EntryKind::Operation,
        check,
    }]
}
fn rows(store: &dyn DomainStore, entity: &str) -> Vec<Row> {
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
        body: Payload::Json(encode(&query).unwrap()),
    }))
    .unwrap();
    let page: Page<Row> = output.body.decode().unwrap();
    page.rows.into_iter().map(|(row, _)| row).collect()
}
fn message(id: &str, source: Source, workspace: &str, conv: &str, status: &str, at: i64) -> Row {
    record(&[
        (
            "source",
            string(if source == Source::ChanMessage {
                "chan_message"
            } else {
                "mail_message"
            }),
        ),
        ("id", string(id)),
        ("workspace_id", string(workspace)),
        ("conv", string(conv)),
        ("status", string(status)),
        ("at", Cell::Int(at)),
    ])
}
fn command(id: &str, source: Source, workspace: &str) -> Mutation {
    Mutation::Op(
        SyncPendingMembership::enqueue_digestible(source, id.into(), workspace.into())
            .unwrap()
            .to_wire()
            .unwrap(),
    )
}
fn submit(h: &dyn Harness, store: &dyn DomainStore, id: &str, mutations: Vec<Mutation>) {
    let unit = super::suite::uow(h, Domain::Comms, id, mutations);
    block_on(store.commit(unit)).unwrap();
}
/// Covers intake, repeat, receipt scope, moves, recalls, deletions, mail and FIFO metadata.
pub fn check(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let source = Source::ChanMessage;
    for (id, at) in [("a", 30), ("b", 10), ("c", 20)] {
        submit(
            h,
            store.as_ref(),
            id,
            vec![
                Mutation::Insert {
                    entity: SOURCE.name.into(),
                    row: message(id, source, "ws", "space:old", "received", at),
                },
                command(id, source, "ws"),
            ],
        );
    }
    let pending = rows(store.as_ref(), PENDING.name);
    assert_eq!(pending.len(), 3);
    let conv = rows(store.as_ref(), CONVS.name);
    assert_eq!(
        (
            int(&conv[0], "messages"),
            int(&conv[0], "oldest"),
            int(&conv[0], "newest")
        ),
        (3, 10, 30)
    );
    submit(
        h,
        store.as_ref(),
        "repeat",
        vec![command("a", source, "ws")],
    );
    assert_eq!(rows(store.as_ref(), PENDING.name), pending);
    let acknowledge =
        SyncPendingMembership::dequeue_on_receipt("space:old", "b", "ws".into()).unwrap();
    submit(
        h,
        store.as_ref(),
        "ack",
        vec![
            Mutation::Insert {
                entity: RECEIPT.name.into(),
                row: record(&[
                    ("workspace_id", string("ws")),
                    ("conv", string("space:old")),
                    ("message_id", string("b")),
                ]),
            },
            Mutation::Op(acknowledge.to_wire().unwrap()),
        ],
    );
    submit(
        h,
        store.as_ref(),
        "no-requeue",
        vec![command("b", source, "ws")],
    );
    assert_eq!(rows(store.as_ref(), PENDING.name).len(), 2);
    let conv = rows(store.as_ref(), CONVS.name);
    assert_eq!((int(&conv[0], "oldest"), int(&conv[0], "newest")), (20, 30));
    submit(
        h,
        store.as_ref(),
        "move",
        vec![
            Mutation::Upsert {
                entity: SOURCE.name.into(),
                row: message("a", source, "other", "space:new", "sent", 5),
            },
            command("a", source, "other"),
        ],
    );
    let moved = rows(store.as_ref(), PENDING.name);
    assert_eq!(moved.len(), 2);
    let moved_a = moved
        .iter()
        .find(|row| text(row, "message_id") == "a")
        .unwrap();
    assert_eq!(text(moved_a, "workspace_id"), "other");
    assert!(int(moved_a, "position") > pending.iter().map(|r| int(r, "position")).max().unwrap());
    submit(
        h,
        store.as_ref(),
        "recall",
        vec![
            Mutation::Upsert {
                entity: SOURCE.name.into(),
                row: message("c", source, "ws", "space:old", "recalled", 20),
            },
            command("c", source, "ws"),
        ],
    );
    assert_eq!(rows(store.as_ref(), CONVS.name).len(), 1);
    submit(
        h,
        store.as_ref(),
        "delete",
        vec![
            Mutation::Delete {
                entity: SOURCE.name.into(),
                key: vec![string("chan_message"), string("a")],
            },
            command("a", source, "other"),
        ],
    );
    assert!(rows(store.as_ref(), PENDING.name).is_empty());
    assert!(rows(store.as_ref(), CONVS.name).is_empty());
    submit(
        h,
        store.as_ref(),
        "mail",
        vec![
            Mutation::Insert {
                entity: SOURCE.name.into(),
                row: message(
                    "mail",
                    Source::MailMessage,
                    "ws",
                    "mail_thread:t",
                    "queued",
                    40,
                ),
            },
            command("mail", Source::MailMessage, "ws"),
        ],
    );
    assert!(rows(store.as_ref(), PENDING.name).is_empty());
    submit(
        h,
        store.as_ref(),
        "mail-sent",
        vec![
            Mutation::Upsert {
                entity: SOURCE.name.into(),
                row: message(
                    "mail",
                    Source::MailMessage,
                    "ws",
                    "mail_thread:t",
                    "sent",
                    40,
                ),
            },
            command("mail", Source::MailMessage, "ws"),
        ],
    );
    let mail = rows(store.as_ref(), PENDING.name);
    assert_eq!(mail.len(), 1);
    assert!(int(&mail[0], "position") > int(moved_a, "position"));
    let reconcile = SyncPendingMembership::sync_pending_conversation(
        Source::MailMessage,
        "mail".into(),
        "ws".into(),
    );
    submit(
        h,
        store.as_ref(),
        "reconcile",
        vec![Mutation::Op(reconcile.to_wire().unwrap())],
    );
    assert_eq!(rows(store.as_ref(), PENDING.name), mail);
    // Late uniqueness failure rolls back both source changes and derived inbox metadata.
    let unit = super::suite::uow(
        h,
        Domain::Comms,
        "digest-atomic",
        vec![
            Mutation::Upsert {
                entity: SOURCE.name.into(),
                row: message(
                    "new",
                    Source::MailMessage,
                    "ws",
                    "mail_thread:t",
                    "received",
                    50,
                ),
            },
            command("new", Source::MailMessage, "ws"),
            Mutation::Insert {
                entity: SOURCE.name.into(),
                row: message(
                    "mail",
                    Source::MailMessage,
                    "ws",
                    "mail_thread:t",
                    "sent",
                    40,
                ),
            },
        ],
    );
    assert!(matches!(
        block_on(store.commit(unit)),
        Err(CommitError::Unique { .. })
    ));
    assert_eq!(rows(store.as_ref(), PENDING.name), mail);
}
