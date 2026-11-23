use crate::{Result, Store};

pub fn credential_item(store: &Store, con: &str) -> Result<String> {
    store.reader(|c| {
        c.query_row(
            "SELECT credential_item FROM connections WHERE id = ?",
            [con],
            |r| r.get::<_, String>(0),
        )
        .map_err(crate::StoreError::from)
    })
}

pub fn account_connection(store: &Store, acc: &str) -> Result<String> {
    store.reader(|c| {
        c.query_row(
            "SELECT id FROM connections WHERE account_id = ?",
            [acc],
            |r| r.get::<_, String>(0),
        )
        .map_err(crate::StoreError::from)
    })
}
