use super::*;

pub fn output_channel_preference_manual_space_id_learned_space_id(
    store: &Store,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT manual_space_id,learned_space_id FROM output_channel_preferences WHERE party_id='output-person'"#,
        [],
    )
}

pub fn count_output_channel_preferences(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_channel_preferences WHERE manual_space_id='sp_a' OR learned_space_id='sp_a'"#,
        [],
    )
}

pub fn count_output_channel_preferences_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_channel_preferences WHERE party_id='ann'"#,
        [],
    )
}

pub fn count_output_channel_preferences_count_2(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_channel_preferences WHERE party_id='bob' AND manual_space_id='sp_b' AND learned_space_id IS NULL"#,
        [],
    )
}
