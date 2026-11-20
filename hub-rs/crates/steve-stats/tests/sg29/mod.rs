use rusqlite::params;
use serde_json::{Value, json};
use steve_stats::{metrics::Query, refresh};
use steve_store::{NoPublisher, Store, WS};
pub const AT: i64 = 1_704_067_200_000;
pub const DAY: &str = "2024-01-01";
pub const DAY2: &str = "2024-01-02";
pub const DAY_MS: i64 = 86_400_000;

pub struct U<'a> {
    pub ws: &'a str,
    pub ts: i64,
    pub source: &'a str,
    pub purpose: &'a str,
    pub model: &'a str,
    pub turn: Option<&'a str>,
    pub session: Option<&'a str>,
    pub item: Option<&'a str>,
    pub bot: Option<&'a str>,
    pub tokens: [Option<i64>; 4],
    pub cost: Option<f64>,
    pub detail: Value,
}
impl<'a> U<'a> {
    pub fn new(model: &'a str, tokens: [Option<i64>; 4]) -> Self {
        Self {
            ws: WS,
            ts: AT + 5_000,
            source: "agent",
            purpose: "turn",
            model,
            turn: Some("t1"),
            session: Some("s1"),
            item: Some("ST-1"),
            bot: Some("dev"),
            tokens,
            cost: None,
            detail: json!({"scope":"all"}),
        }
    }
}
pub struct F {
    pub _dir: tempfile::TempDir,
    pub store: Store,
}
impl F {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store.with_conn(|c|{c.execute_batch(&format!("
 INSERT INTO workspaces(id,name,created) VALUES('other','Other',0);
 INSERT INTO settings(workspace_id,key,value,updated) VALUES('default','stats.tz','\"UTC\"',0);
 INSERT INTO bots(id,workspace_id,name,main,harness,model,updated) VALUES('dev','default','Dev',0,'custom','bot-fallback',0);
 INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES('dev','default',0);
 INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES
 ('ST-1','default',1,'done item','done','done','dev',{at},{done}),
 ('ST-2','default',2,'open item','run','run','dev',{day2},{day2});
 INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,model,cwd,status,created,updated) VALUES
 ('s1','default','ST-1','dev','custom','session-fallback','.','idle',{at},{done}),
 ('s2','default','ST-2','dev','custom','session-fallback','.','idle',{day2},{day2});
 INSERT INTO agent_turns(id,workspace_id,session_id,item_id,bot_id,harness,attempt,started,ended,model,tools,tool_errors,tool_kinds,stop_reason) VALUES
 ('t1','default','s1','ST-1','dev','custom',0,{start},{end},'m-a',2,1,'{{\"execute\":2}}','end_turn'),
 ('t2','default','s2','ST-2','dev','custom',0,{start2},{end2},NULL,0,0,'{{}}','end_turn');
 INSERT INTO item_marks(workspace_id,item_id,ts,kind,from_v,to_v,actor,detail) VALUES
 ('default','ST-1',{at},'status',NULL,'run','me','{{}}'),
 ('default','ST-2',{day2},'status',NULL,'run','me','{{}}'),
 ('default','ST-1',{done1},'status','run','done','me','{{}}'),
 ('default','ST-1',{reopen},'status','done','run','me','{{}}'),
 ('default','ST-1',{done},'status','run','done','me','{{}}');
 INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,item_id,question,status,result,requested_by,created,resolved) VALUES
 ('q1','default','ask','item','ST-1','ST-1','q','resolved','{{}}','dev',{qstart},{qend}),
 ('q2','default','approve','item','ST-1','ST-1','q','resolved','{{}}','dev',{qstart2},{qend2}),
 ('auto','default','ask','item','ST-1','ST-1','auto','resolved','{{\"decided_by\":\"yolo\"}}','dev',{start},{autoend}),
 ('orphan','default','ask','other','x',NULL,'orphan','resolved','{{}}','dev',{qstart},{orphanend});
 ",at=AT,day2=AT+DAY_MS,done=AT+12_000,start=AT+1_000,end=AT+5_000,start2=AT+DAY_MS+1_000,end2=AT+DAY_MS+2_000,done1=AT+10_000,reopen=AT+11_000,qstart=AT+2_000,qend=AT+6_000,qstart2=AT+4_000,qend2=AT+8_000,autoend=AT+1_500,orphanend=AT+3_000))?;
 c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES('default','stats.prices',?,0)",[json!([{"match":"m-a","input":1,"output":2,"cache_read":3,"cache_write":4}]).to_string()])?;Ok(())}).unwrap();
        let f = Self { _dir: dir, store };
        let mut all = U::new("m-a", [Some(100), Some(20), Some(10), Some(5)]);
        all.cost = Some(0.7);
        f.usage(all);
        let mut cost = U::new("m-a", [None; 4]);
        cost.cost = Some(0.2);
        cost.detail = json!({"scope":"main"});
        f.usage(cost);
        let mut zero = U::new("unpriced-sg29", [Some(0); 4]);
        zero.source = "gateway";
        zero.purpose = "route";
        zero.turn = None;
        zero.session = None;
        zero.cost = Some(0.0);
        f.usage(zero);
        f.rebuild();
        f
    }
    pub fn usage(&self, u: U<'_>) -> i64 {
        self.store.with_conn(|c|{c.execute("INSERT INTO usage_records(workspace_id,ts,day,source,purpose,harness,model,turn_id,session_id,item_id,bot_id,input,output,cache_read,cache_write,cost_reported,ok,attempts,detail) VALUES(?,?,?,?,?,'custom',?,?,?,?,?,?,?,?,?,?,1,1,?)",params![u.ws,u.ts,steve_stats::day(u.ts,Some(chrono_tz::UTC)),u.source,u.purpose,u.model,u.turn,u.session,u.item,u.bot,u.tokens[0],u.tokens[1],u.tokens[2],u.tokens[3],u.cost,u.detail.to_string()])?;Ok(c.last_insert_rowid())}).unwrap()
    }
    pub fn rebuild(&self) {
        self.store
            .transaction(|c| refresh::rebuild_in(c, WS))
            .unwrap();
        refresh::refresh_all(&self.store).unwrap();
    }
    pub fn q(&self) -> Query {
        Query {
            workspace_id: WS.into(),
            from: DAY.into(),
            to: DAY2.into(),
            ..Default::default()
        }
    }
    pub fn cube(&self, ws: &str) -> Vec<String> {
        self.store.reader(|c|{let mut s=c.prepare("SELECT json_array(day,source,purpose,harness,model,input,output,cache_read,cache_write,reasoning,hit_base,cost_reported,calls,failed_calls,turns,turn_ms,turn_errors,retries,interrupted,tools,tool_errors,tool_kinds) FROM stat_daily WHERE workspace_id=? ORDER BY day,source,purpose,harness,model,bot_id,project_id")?;Ok::<_,steve_store::StoreError>(s.query_map([ws],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?)}).unwrap()
    }
}
pub fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}
