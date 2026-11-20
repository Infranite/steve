//! Conformance-only authorizer evidence. This proves tables touched by executed
//! scenarios, never absence of untested access. Legacy raw-SQL grants are excluded.
//! UPDATE records the table only; column ownership is not part of this guard.
use rusqlite::{
    Connection,
    hooks::{AuthAction, AuthContext, Authorization},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Access {
    pub operation: String,
    pub context: String,
    pub table: String,
    pub mode: &'static str,
}
thread_local! { static CURRENT: RefCell<Option<(String, String)>> = const { RefCell::new(None) }; }
fn records() -> &'static Mutex<BTreeSet<Access>> {
    static LOG: OnceLock<Mutex<BTreeSet<Access>>> = OnceLock::new();
    LOG.get_or_init(Mutex::default)
}
fn mapping() -> &'static BTreeMap<&'static str, (&'static str, &'static str)> {
    static MAP: OnceLock<BTreeMap<&str, (&str, &str)>> = OnceLock::new();
    MAP.get_or_init(|| {
        include_str!("../schema/tablemap.tsv")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|l| {
                let mut fields = l.split('\t');
                (
                    fields.next().unwrap(),
                    (fields.next().unwrap(), fields.next().unwrap()),
                )
            })
            .collect()
    })
}
pub(crate) fn table_context(table: &str) -> String {
    mapping()
        .get(table)
        .map(|(d, c)| if *c == "-" { *d } else { *c })
        .unwrap_or("unmapped")
        .to_string()
}
pub(crate) fn named_context(name: &str, domain: steve_storage_api::Domain) -> String {
    let mut parts = name.split('.');
    let first = parts.next().unwrap_or_default();
    if first == "core" {
        parts.next().unwrap_or("unmapped").into()
    } else if first == "verify" {
        "verify".into()
    } else {
        crate::domains::domain_name(domain).into()
    }
}
pub(crate) struct Operation(Option<(String, String)>);
impl Operation {
    pub(crate) fn enter(name: impl Into<String>, context: impl Into<String>) -> Self {
        Self(CURRENT.with(|v| v.replace(Some((name.into(), context.into())))))
    }
    pub(crate) fn on(c: &Connection, name: impl Into<String>, context: impl Into<String>) -> Self {
        attach(c);
        Self::enter(name, context)
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        CURRENT.with(|v| {
            v.replace(self.0.take());
        });
    }
}
/// Every operation boundary clears cached statements. Disabling caching also covers
/// export snapshots that retain a connection between batches under another thread.
fn attach(c: &Connection) {
    c.flush_prepared_statement_cache();
    c.set_prepared_statement_cache_capacity(0);
    c.authorizer(Some(|ctx: AuthContext<'_>| {
        let (table, mode) = match ctx.action {
            AuthAction::Read { table_name, .. } => (table_name, "read"),
            AuthAction::Insert { table_name }
            | AuthAction::Update { table_name, .. }
            | AuthAction::Delete { table_name } => (table_name, "write"),
            _ => return Authorization::Allow,
        };
        CURRENT.with(|v| {
            if let Some((operation, context)) = v.borrow().as_ref() {
                let owner = table_context(table);
                let context = if owner == "contract" || context == "$entity" {
                    &owner
                } else {
                    context
                };
                records()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(Access {
                        operation: operation.clone(),
                        context: context.clone(),
                        table: table.into(),
                        mode,
                    });
            }
        });
        Authorization::Allow
    }))
    .expect("conformance authorizer installation");
}
pub fn take() -> BTreeSet<Access> {
    std::mem::take(&mut *records().lock().unwrap_or_else(|e| e.into_inner()))
}
/// Unknown tables fail closed. The checked-in exception list must exactly equal actual
/// cross-context evidence: stale exceptions fail too. Unassigned '?' contexts remain
/// visible in the catalogue until their domain owners assign them.
pub fn cross_context(accesses: &BTreeSet<Access>) -> BTreeSet<String> {
    accesses
        .iter()
        .filter(|a| {
            table_context(&a.table) != "contract"
                && (!mapping().contains_key(a.table.as_str())
                    || a.context != table_context(&a.table))
        })
        .map(|a| format!("{}\t{}\t{}", a.context, a.table, a.mode))
        .collect()
}
pub fn expected(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(str::to_owned)
        .collect()
}
