use super::*;

pub fn count_pragma_foreign_key_check(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM pragma_foreign_key_check"#,
        [],
    )
}
