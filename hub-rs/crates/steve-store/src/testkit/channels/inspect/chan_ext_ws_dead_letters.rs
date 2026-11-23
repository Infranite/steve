use super::*;

pub fn chan_ext_ws_dead_letter_source_id_payload_by_connection(
    store: &Store,
    connection_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT source_id, payload FROM chan_ext_ws_dead_letters WHERE connection_id = ?"#,
        [connection_id],
    )
}

pub fn chan_ext_ws_dead_letter_payload_by_connection(
    store: &Store,
    connection_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT payload FROM chan_ext_ws_dead_letters WHERE connection_id = ?"#,
        [connection_id],
    )
}

pub fn chan_ext_ws_dead_letter_source_id_by_connection(
    store: &Store,
    connection_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT source_id FROM chan_ext_ws_dead_letters WHERE connection_id = ?"#,
        [connection_id],
    )
}
