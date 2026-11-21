//! Pure command planning. W3.3s connects these decisions to the durable coordinator.
#![allow(dead_code)] // G4-W33A-UNWIRED if W3.3s is delayed; remove upon ledger integration.
pub mod driver;
pub mod machine;
pub mod policy;
