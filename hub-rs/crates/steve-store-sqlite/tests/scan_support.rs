#![allow(dead_code)]
use std::sync::Arc;
use steve_storage_api::{entity::*, query::*, *};
use steve_store_sqlite::{engine::Engine, open};
pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub engine: Arc<Engine>,
    pub writer: rusqlite::Connection,
    pub desc: EntityDesc,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let engine = Arc::new(open::open_runtime(temp.path()).unwrap());
        let writer = rusqlite::Connection::open(engine.path()).unwrap();
        writer.execute_batch("CREATE TABLE rec(id INTEGER PRIMARY KEY,score INTEGER,label TEXT COLLATE NOCASE NOT NULL,rev INTEGER NOT NULL);CREATE INDEX rec_score ON rec(score,id); CREATE INDEX rec_label ON rec(label COLLATE BINARY,id);").unwrap();
        let desc = EntityDesc {
            name: "rec".into(),
            domain: Domain::Core,
            key: vec!["id".into()],
            fields: vec![
                FieldDesc {
                    name: "id".into(),
                    ty: FieldType::Int,
                    nullable: false,
                },
                FieldDesc {
                    name: "score".into(),
                    ty: FieldType::Int,
                    nullable: true,
                },
                FieldDesc {
                    name: "label".into(),
                    ty: FieldType::Text,
                    nullable: false,
                },
            ],
            indexes: vec![
                IndexDesc {
                    name: "score".into(),
                    fields: vec!["score".into()],
                    unique: false,
                },
                IndexDesc {
                    name: "label".into(),
                    fields: vec!["label".into()],
                    unique: false,
                },
            ],
            kind: EntityKind::Mutable,
            retention: None,
        };
        Self {
            temp,
            engine,
            writer,
            desc,
        }
    }
    pub fn seed(&self, n: i64) {
        for id in 1..=n {
            self.writer
                .execute(
                    "INSERT INTO rec VALUES(?1,?2,?3,?4)",
                    rusqlite::params![id, id / 3, format!("row{id}"), id],
                )
                .unwrap();
        }
    }
    pub fn q(&self, name: &str, body: impl serde::Serialize) -> AnyQuery {
        request(&self.desc, name, body)
    }
    pub fn get(&self, id: i64) -> AnyQuery {
        self.q(
            "get",
            serde_json::json!({"key":{"entity":"rec","values":[Cell::Int(id)]}}),
        )
    }
    pub fn scan(
        &self,
        limit: u32,
        budget: u32,
        resume: Option<ResumeToken>,
        filter: Vec<Pred>,
    ) -> AnyQuery {
        self.q("scan",serde_json::json!({"index":{"entity":"rec","name":"score"},"range":KeyRange::default(),"filter":filter,"order":Order::Asc,"limit":limit,"budget":ScanBudget(budget),"resume":resume}))
    }
    pub fn run<T: serde::de::DeserializeOwned>(&self, q: AnyQuery) -> Result<T, Error> {
        let conn = self.engine.read_conn()?;
        steve_store_sqlite::scan::query(&conn, &self.desc, q)?
            .body
            .decode()
    }
}
pub fn request(e: &EntityDesc, name: &str, body: impl serde::Serialize) -> AnyQuery {
    AnyQuery {
        name: name.into(),
        domain: e.domain,
        body: Payload::Json(encode(&body).unwrap()),
    }
}
pub fn id(row: &Row) -> i64 {
    match row.0.iter().find(|(n, _)| n == "id").unwrap().1 {
        Cell::Int(n) => n,
        _ => panic!("id type"),
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
