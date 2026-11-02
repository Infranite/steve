//! What disconnecting a mailbox removes (`ConnectionUsage::on_disconnect`): its drafts, messages,
//! attachments, threads, folders, settings and source settings rows — all in the disconnect
//! transaction, before the connection row goes — and the raw files.

use steve_modules::accounts::connections::{ConnectionUsage, Usage};
use steve_modules::vault::VaultResult;
use steve_store::Tx;

use crate::config::MailConfig;
use crate::repo;
use crate::store::raw_abs_path;

pub struct MailUsage {
    cfg: MailConfig,
}

impl MailUsage {
    pub fn new(cfg: MailConfig) -> Self {
        Self { cfg }
    }
}

impl ConnectionUsage for MailUsage {
    fn usage(&self, _: &rusqlite::Connection, _: &str, _: &str) -> VaultResult<Usage> {
        // mail is not a flow, space or event source of its own; the signals module lists the source
        Ok(Usage::default())
    }

    fn on_disconnect(&self, tx: &Tx, _ws: &str, connection_id: &str) -> VaultResult<()> {
        let paths = repo::raw_paths(tx, connection_id)?;
        repo::delete_connection(tx, connection_id)?;
        // created by the mail sync for the folder scopes; the signals module may delete it as well
        tx.prepare_cached("DELETE FROM source_settings WHERE connection_id = ?")
            .and_then(|mut s| s.execute([connection_id]))
            .map_err(crate::MailError::from)?;
        // files last: if the transaction rolls back after this, the rows survive without raw files,
        // which `refetch` handles like expired mail
        for p in paths {
            if let Some(path) = raw_abs_path(self.cfg.home(), &p)
                && let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(error = %e, "could not delete a mail file");
            }
        }
        Ok(())
    }
}
