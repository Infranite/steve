//! Database fixtures and invariants for integration tests.
pub mod channels;
pub mod digest;
pub mod flow;
pub mod knowledge;
pub mod stats;

pub mod hub;
pub mod modules;
pub mod vault;

/// The active write connection for driver-level tests in this crate.
pub fn sqlite_writer<'a>(write: &crate::W<'a>) -> &'a rusqlite::Connection {
    write.conn
}

/// Direct SQLite access for the engine and migration integration tests in this crate.
pub trait SqliteFixtures {
    fn fixture_write<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> crate::Result<T>,
    ) -> crate::Result<T>;
    fn fixture_read<T, E: From<crate::StoreError>>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, E>,
    ) -> Result<T, E>;
}

impl SqliteFixtures for crate::Store {
    fn fixture_write<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> crate::Result<T>,
    ) -> crate::Result<T> {
        self.write_locked(f)
    }

    fn fixture_read<T, E: From<crate::StoreError>>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        self.sqlite_read(f)
    }
}
