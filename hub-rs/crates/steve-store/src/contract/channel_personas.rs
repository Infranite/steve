//! Personas retain their visible identity when their Bot is replaced.

use sea_query::{Expr, ExprTrait, Order, Query};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contract::{Json, Row};
use crate::{Read, Result, W};

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

struct BotPersona {
    id: String,
    name: String,
    intro: String,
    soul: String,
    reveal: Reveal,
    signature: String,
    revision: i64,
}

impl BotPersona {
    fn read(row: &Row<'_>) -> Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            intro: row.get(2)?,
            soul: row.get(3)?,
            reveal: row.get::<Json<Reveal>>(4)?.0,
            signature: row.get(5)?,
            revision: row.get(6)?,
        })
    }

    fn snapshot(&self, bot: &str) -> Value {
        json!({
            "name": self.name, "bot_id": bot, "intro": self.intro, "soul": self.soul,
            "reveal": self.reveal, "signature": self.signature,
        })
    }
}

fn with_bot(r: &mut impl Read, ws: &str, bot: &str) -> Result<Vec<BotPersona>> {
    r.all(
        Query::select()
            .columns([
                "id",
                "name",
                "intro",
                "soul",
                "reveal",
                "signature",
                "revision",
            ])
            .from("personas")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("bot_id").eq(bot))
            .and_where(Expr::col("deleted").eq(0_i64))
            .order_by("id", Order::Asc),
        BotPersona::read,
    )
}

pub fn reassign_bot(w: &mut W<'_>, ws: &str, bot: &str, heir: &str, at: i64) -> Result<usize> {
    let personas = with_bot(w, ws, bot)?;
    for persona in &personas {
        let version = persona.revision + 1;
        w.exec(
            Query::update()
                .table("personas")
                .values([
                    ("bot_id", heir.into()),
                    ("revision", version.into()),
                    ("updated", at.into()),
                ])
                .and_where(Expr::col("id").eq(persona.id.as_str())),
        )?;
        super::audit::revise(
            w,
            ws,
            "persona",
            &persona.id,
            version,
            &persona.snapshot(bot),
            &persona.snapshot(heir),
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
