//! Contact and attribute types shared across modules.

use steve_store::base::{Viewer, Visibility};
use steve_store::{Attributes, Contacts, Read, Result, W};

use crate::Owned;
pub use steve_store::{
    AttrDef, AttrValue, ContactQuery, NewDef, NewParty, NewValue, Party, PartyPatch, TeamRef,
};

pub const PARTY: &str = "party";

pub fn get(c: &mut impl Read, id: &str) -> Result<Option<Party>> {
    Contacts::new(c).party(id)
}
pub fn me_of(c: &mut impl Read, ws: &str, user: &str) -> Result<Option<Party>> {
    Contacts::new(c).me(ws, user)
}
pub fn self_org(c: &mut impl Read, ws: &str) -> Result<Option<Party>> {
    Contacts::new(c).own_org(ws)
}
pub fn contacts(c: &mut impl Read, viewer: &Viewer, filters: &ContactQuery) -> Result<Vec<Party>> {
    Contacts::new(c).contacts(viewer, filters)
}
pub fn orgs(c: &mut impl Read, viewer: &Viewer) -> Result<Vec<Party>> {
    Contacts::new(c).organizations(viewer)
}
pub fn values(c: &mut impl Read, ws: &str, party: &str) -> Result<Vec<AttrValue>> {
    Attributes(c).values(ws, party)
}

impl Owned for Party {
    fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    fn visibility(&self) -> Option<&Visibility> {
        Some(&self.visibility)
    }
    fn created_by(&self) -> &str {
        &self.created_by
    }
}

pub fn insert(tx: &mut W<'_>, ws: &str, p: &NewParty) -> Result<Party> {
    tx.contacts().insert(ws, p)
}

pub fn update(tx: &mut W<'_>, id: &str, p: &PartyPatch, revision: Option<i64>) -> Result<bool> {
    tx.contacts().update(id, p, revision)
}

pub fn like(q: &str) -> String {
    let mut s = String::from("%");
    for ch in q.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(ch);
    }
    s.push('%');
    s
}

pub fn value_insert(tx: &mut W<'_>, ws: &str, v: &NewValue) -> Result<AttrValue> {
    Attributes(tx).insert_value(ws, v)
}
