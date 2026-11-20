//! Pure command planning. W3.3s connects these decisions to the durable coordinator.
#![allow(dead_code)] // Deliberately unconnected until the ledger integration lands.
pub mod driver;
pub mod machine;
pub mod policy;
