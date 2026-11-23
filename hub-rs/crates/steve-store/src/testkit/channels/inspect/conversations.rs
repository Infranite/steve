use super::*;

pub fn turn_text(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT text FROM messages WHERE space = 'notify' ORDER BY created DESC LIMIT 1"#,
        [],
    )
}

pub fn turn_count(store: &Store, value: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM messages WHERE space = 'notify' AND json_extract(meta, '$.notice.kind') = ?"#,
        [value],
    )
}

pub fn turn_text_value(store: &Store, value: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT text FROM messages WHERE space = 'notify' AND json_extract(meta, '$.notice.kind') = ?"#,
        [value],
    )
}

pub fn count_messages(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM messages WHERE space = 'notify' AND json_extract(meta, '$.notice.kind') = 'security'"#,
        [],
    )
}

pub fn turn_count_value(store: &Store, value: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM messages WHERE space = 'steve' AND json_extract(meta, '$.notice.kind') = ?"#,
        [value],
    )
}

pub fn turn_outcome(store: &Store, id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT json_extract(meta, '$.chan_gen.outcome') FROM messages WHERE id = ?"#,
        [id],
    )
}
