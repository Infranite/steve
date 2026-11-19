//! Final schema for the single-file layout. Cross-domain debt must be cleared before splitting.

pub const CORE: &str = include_str!("../schema/core/0001_baseline.sql");
pub const STREAM: &str = include_str!("../schema/stream/0001_baseline.sql");
pub const COMMS: &str = include_str!("../schema/comms/0001_baseline.sql");
pub const CONTRACT: &str = include_str!("../schema/contract/0001.sql");
