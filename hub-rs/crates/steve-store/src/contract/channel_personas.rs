//! Personas retain their visible identity when their Bot is replaced.

use sea_query::{Expr, ExprTrait, Order, Query};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contract::{Json, Q, Row};
use crate::{Read, Result, W};

pub mod bindings;

/// What a persona tells about itself. A field omitted from the policy is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reveal {
    pub name: bool,
    pub role: bool,
    pub ai: bool,
    pub steve: bool,
    pub owner: bool,
}

impl Reveal {
    pub const ALL: Self = Self {
        name: true,
        role: true,
        ai: true,
        steve: true,
        owner: true,
    };

    pub const NEW: Self = Self {
        name: true,
        role: true,
        ai: true,
        steve: false,
        owner: false,
    };
}

impl Default for Reveal {
    fn default() -> Self {
        Self::ALL
    }
}

pub fn name(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("name")
            .from("personas")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )
}

/// The stored face a Bot presents in a channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Persona {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub avatar: Value,
    pub bot_id: String,
    pub intro: String,
    pub soul: String,
    pub reveal: Reveal,
    pub signature: String,
    pub is_default: bool,
    pub deleted: bool,
    pub revision: i64,
    pub created: i64,
    pub updated: i64,
}

const COLUMNS: [&str; 14] = [
    "id",
    "workspace_id",
    "name",
    "avatar",
    "bot_id",
    "intro",
    "soul",
    "reveal",
    "signature",
    "is_default",
    "deleted",
    "revision",
    "created",
    "updated",
];

fn decode(row: &Row<'_>) -> Result<Persona> {
    Ok(Persona {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        avatar: row.get::<Json<Value>>(3)?.0,
        bot_id: row.get(4)?,
        intro: row.get(5)?,
        soul: row.get(6)?,
        reveal: row.get::<Json<Reveal>>(7)?.0,
        signature: row.get(8)?,
        is_default: row.get::<i64>(9)? != 0,
        deleted: row.get::<i64>(10)? != 0,
        revision: row.get(11)?,
        created: row.get(12)?,
        updated: row.get(13)?,
    })
}

pub fn get(r: &mut impl Read, id: &str) -> Result<Option<Persona>> {
    r.one(
        Query::select()
            .columns(COLUMNS)
            .from("personas")
            .and_where(Expr::col("id").eq(id)),
        decode,
    )
}

fn live(ws: &str) -> sea_query::SelectStatement {
    Query::select()
        .columns(COLUMNS)
        .from("personas")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("deleted").eq(0i64))
        .take()
}

pub fn list(r: &mut impl Read, ws: &str) -> Result<Vec<Persona>> {
    r.all(
        live(ws).order_by_columns([
            ("is_default", Order::Desc),
            ("created", Order::Asc),
            ("id", Order::Asc),
        ]),
        decode,
    )
}

pub fn default_persona(r: &mut impl Read, ws: &str) -> Result<Option<Persona>> {
    r.one(
        live(ws)
            .and_where(Expr::col("is_default").eq(1i64))
            .order_by("created", Order::Asc)
            .limit(1),
        decode,
    )
}

pub struct NewPersona<'a> {
    pub workspace: &'a str,
    pub name: &'a str,
    pub bot: &'a str,
    pub intro: &'a str,
    pub soul: &'a str,
    pub reveal: Reveal,
    pub signature: &'a str,
    pub is_default: bool,
    pub now: i64,
}

pub fn create(w: &mut W<'_>, input: &NewPersona<'_>) -> Result<Persona> {
    let id = crate::uid("per");
    w.exec(
        Query::insert()
            .into_table("personas")
            .columns([
                "id",
                "workspace_id",
                "name",
                "avatar",
                "bot_id",
                "intro",
                "soul",
                "reveal",
                "signature",
                "is_default",
                "created",
                "updated",
            ])
            .values_panic([
                id.as_str().into(),
                input.workspace.into(),
                input.name.into(),
                json!({"seed": input.name}).to_string().into(),
                input.bot.into(),
                input.intro.into(),
                input.soul.into(),
                serde_json::to_string(&input.reveal)?.into(),
                input.signature.into(),
                i64::from(input.is_default).into(),
                input.now.into(),
                input.now.into(),
            ]),
    )?;
    crate::repo::must(get(w, &id)?)
}

pub fn update(w: &mut W<'_>, persona: &Persona) -> Result<()> {
    w.exec(
        Query::update()
            .table("personas")
            .value("name", persona.name.as_str())
            .value("bot_id", persona.bot_id.as_str())
            .value("intro", persona.intro.as_str())
            .value("soul", persona.soul.as_str())
            .value("reveal", serde_json::to_string(&persona.reveal)?)
            .value("signature", persona.signature.as_str())
            .value("revision", persona.revision)
            .value("updated", persona.updated)
            .and_where(Expr::col("id").eq(persona.id.as_str())),
    )?;
    Ok(())
}

pub fn retire(w: &mut W<'_>, id: &str, now: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table("personas")
            .value("deleted", 1i64)
            .value("revision", Expr::col("revision").add(1i64))
            .value("updated", now)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

impl Persona {
    pub fn snapshot(&self) -> Value {
        json!({
            "name": self.name, "bot_id": self.bot_id, "intro": self.intro, "soul": self.soul,
            "reveal": self.reveal, "signature": self.signature,
        })
    }
}

pub fn reassign_bot(w: &mut W<'_>, ws: &str, bot: &str, heir: &str, at: i64) -> Result<usize> {
    let mut personas = w.all(
        live(ws)
            .and_where(Expr::col("bot_id").eq(bot))
            .order_by("id", Order::Asc),
        decode,
    )?;
    for persona in &mut personas {
        let before = persona.snapshot();
        persona.bot_id = heir.into();
        persona.revision += 1;
        persona.updated = at;
        update(w, persona)?;
        super::audit::revise(
            w,
            ws,
            "persona",
            &persona.id,
            persona.revision,
            &before,
            &persona.snapshot(),
            "steve",
        )?;
        super::audit::record(
            w,
            ws,
            "steve",
            "persona.bot_moved",
            &persona.id,
            &json!({"from": bot, "to": heir}),
        )?;
        w.emit(steve_types::DomainEvent::from(
            steve_types::ChannelsEvent::Persona {
                id: persona.id.clone().into(),
                ws: ws.into(),
            },
        ));
    }
    Ok(personas.len())
}
