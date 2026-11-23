use super::*;

pub fn workspace_items(store: &Store, ws: &str) -> Result<Vec<String>> {
    texts(
        store,
        "SELECT id FROM items WHERE workspace_id=? ORDER BY created,rowid",
        [ws],
    )
}
