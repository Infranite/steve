//! Mail adapters. E1: [`imap::ImapAdapter`]; Gmail and Outlook are E2.

pub mod imap;
pub mod utf7;

pub use imap::{ImapAdapter, PRESETS, Preset};
