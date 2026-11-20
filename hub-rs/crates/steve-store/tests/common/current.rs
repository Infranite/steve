//! Current-schema fixtures, independent of archived migration history.
use rusqlite::Connection;
pub fn migrate(c: &mut Connection) -> Result<(), steve_types::Error> {
    use steve_store_sqlite::{compat::StorageState, layout::Layout, migrate, open};
    match open::inspect(c)? {
        StorageState::Empty => migrate::initialize(c, &Layout::single_file()),
        _ => migrate::migrate(c),
    }
}
