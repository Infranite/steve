//! Physical identity and domain placement. These are independent of Hello contract versions.
use crate::open::{db_error, incompatible};
use rusqlite::{Connection, OptionalExtension};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};
use steve_storage_api::Domain;
use steve_types::{Code, Error};

/// Supported physical layout generation (not a domain schema or contract version).
pub const LAYOUT_GENERATION: u32 = 2;
/// Main file for the shared read pool, relative to the storage data directory.
pub const CORE_FILE: &str = "steve.sqlite3";
/// Physical header keys use the core namespace, including on non-core physical files.
pub const META_DOMAIN: &str = "core";
pub const DOMAINS: [Domain; 3] = [Domain::Core, Domain::Stream, Domain::Comms];

/// Placement and identities stored in the core file; filenames are single relative components.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub generation: u32,
    pub min_reader: u32,
    pub store_generation: String,
    pub domains: BTreeMap<Domain, String>,
    pub files: BTreeMap<String, String>,
}
/// Stable namespace used by domain_schema and meta.
pub fn domain_name(domain: Domain) -> &'static str {
    match domain {
        Domain::Core => "core",
        Domain::Stream => "stream",
        Domain::Comms => "comms",
    }
}
/// Read one header value without changing journal mode or schema.
pub fn meta(conn: &Connection, domain: &str, key: &str) -> Result<String, Error> {
    conn.query_row(
        "SELECT value FROM meta WHERE domain=?1 AND key=?2",
        [domain, key],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)?
    .ok_or_else(|| incompatible("storage metadata is incomplete"))
}
/// Upsert one value. Call only inside the caller's metadata transaction.
pub fn put_meta(conn: &Connection, domain: &str, key: &str, value: &str) -> Result<(), Error> {
    conn.execute("INSERT INTO meta(domain,key,value) VALUES(?1,?2,?3) ON CONFLICT(domain,key) DO UPDATE SET value=excluded.value", [domain,key,value]).map_err(db_error)?;
    Ok(())
}
/// Compatibility is checked before all migrations and persistent pragmas.
pub fn check_min_reader(conn: &Connection) -> Result<(), Error> {
    let minimum: u32 = meta(conn, META_DOMAIN, "min_reader")?
        .parse()
        .map_err(|_| incompatible("invalid minimum reader generation"))?;
    if minimum > LAYOUT_GENERATION {
        return Err(incompatible("storage requires a newer reader generation"));
    }
    Ok(())
}
impl Layout {
    /// Generate the single-file wave-2 layout, with unrelated random file/store identities.
    pub fn single_file() -> Self {
        Self {
            generation: LAYOUT_GENERATION,
            min_reader: LAYOUT_GENERATION,
            store_generation: uuid::Uuid::new_v4().to_string(),
            domains: DOMAINS.into_iter().map(|d| (d, CORE_FILE.into())).collect(),
            files: [(CORE_FILE.into(), uuid::Uuid::new_v4().to_string())].into(),
        }
    }
    /// Parse the core manifest. Unknown physical generations fail closed.
    pub fn read(conn: &Connection) -> Result<Self, Error> {
        check_min_reader(conn)?;
        let layout = Self::read_state(conn)?;
        layout.validate()?;
        Ok(layout)
    }
    /// Parse observable identity without applying this binary's reader range.
    pub(crate) fn read_state(conn: &Connection) -> Result<Self, Error> {
        let generation = meta(conn, META_DOMAIN, "layout")?
            .parse()
            .map_err(|_| incompatible("invalid storage layout"))?;
        let min_reader = meta(conn, META_DOMAIN, "min_reader")?
            .parse()
            .map_err(|_| incompatible("invalid minimum reader generation"))?;
        let mut layout = Self {
            generation,
            min_reader,
            store_generation: meta(conn, META_DOMAIN, "store_generation")?,
            domains: BTreeMap::new(),
            files: BTreeMap::new(),
        };
        for domain in DOMAINS {
            let name = domain_name(domain);
            let file = meta(conn, name, "file")?;
            let id = meta(conn, name, "file_id")?;
            if layout.files.get(&file).is_some_and(|old| old != &id) {
                return Err(incompatible("conflicting storage file identities"));
            }
            layout.files.insert(file.clone(), id);
            layout.domains.insert(domain, file);
        }
        layout.validate_shape()?;
        Ok(layout)
    }
    /// Validate before opening/attaching files. No operator-specific absolute paths are stored.
    pub fn validate(&self) -> Result<(), Error> {
        if self.generation != LAYOUT_GENERATION || self.min_reader > LAYOUT_GENERATION {
            return Err(incompatible("unsupported or incomplete storage layout"));
        }
        self.validate_shape()
    }
    pub(crate) fn validate_shape(&self) -> Result<(), Error> {
        if self.store_generation.is_empty()
            || self.domains.len() != DOMAINS.len()
            || self.domains.get(&Domain::Core).map(String::as_str) != Some(CORE_FILE)
        {
            return Err(incompatible("incomplete storage layout"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for (file, id) in &self.files {
            let mut parts = Path::new(file).components();
            if !matches!(parts.next(), Some(Component::Normal(_)))
                || parts.next().is_some()
                || id.is_empty()
                || !ids.insert(id)
            {
                return Err(incompatible(
                    "invalid storage filename or duplicate file identity",
                ));
            }
        }
        if self.domains.values().any(|f| !self.files.contains_key(f))
            || self
                .files
                .keys()
                .any(|f| !self.domains.values().any(|d| d == f))
        {
            return Err(incompatible("unmapped storage file"));
        }
        Ok(())
    }
    /// Stamp one physical file inside the caller's transaction. Core holds every domain's
    /// mapping; a peer holds only its hosted domains plus the physical header.
    pub fn write_file_meta(&self, conn: &Connection, file: &str) -> Result<(), Error> {
        self.validate()?;
        let id = self
            .files
            .get(file)
            .ok_or_else(|| steve_storage_api::error(Code::Invalid, "unknown physical file"))?;
        crate::compat::stamp_reader_fence(conn)?;
        for (key, value) in [
            ("layout", self.generation.to_string()),
            ("min_reader", self.min_reader.to_string()),
            ("store_generation", self.store_generation.clone()),
            ("file_id", id.clone()),
        ] {
            put_meta(conn, META_DOMAIN, key, &value)?;
        }
        for (domain, filename) in &self.domains {
            if file == CORE_FILE || filename == file {
                put_meta(conn, domain_name(*domain), "file", filename)?;
                put_meta(conn, domain_name(*domain), "file_id", &self.files[filename])?;
            }
        }
        Ok(())
    }
    /// Check every file read-only before the writer changes anything in any file.
    pub fn verify_files(&self, dir: &Path) -> Result<(), Error> {
        self.validate()?;
        for (file, expected) in &self.files {
            let conn = crate::open::open_readonly(dir.join(file))?;
            check_min_reader(&conn)?;
            if meta(&conn, META_DOMAIN, "file_id")? != *expected
                || meta(&conn, META_DOMAIN, "layout")? != self.generation.to_string()
                || meta(&conn, META_DOMAIN, "store_generation")? != self.store_generation
            {
                return Err(incompatible(
                    "storage file identity or generation does not match core manifest",
                ));
            }
            for (domain, filename) in &self.domains {
                if filename == file
                    && (meta(&conn, domain_name(*domain), "file")? != *file
                        || meta(&conn, domain_name(*domain), "file_id")? != *expected)
                {
                    return Err(incompatible(
                        "storage file domain placement does not match core manifest",
                    ));
                }
            }
        }
        Ok(())
    }
}
