//! Flag refresh within the already-open mailbox: CONDSTORE delta or bounded classic recheck.
use super::*;

pub(super) struct SelectedMailbox<'a> {
    pub data: &'a async_imap::types::Mailbox,
    pub validity: i64,
}

pub(super) async fn refresh(
    s: &mut Session,
    caps: Caps,
    f: &mut crate::adapter::FolderState,
    mailbox: SelectedMailbox<'_>,
    now: i64,
    sink: &mut dyn MailSink,
    report: &mut SyncReport,
) -> Result<(), AdapterError> {
    let SelectedMailbox { data: mb, validity } = mailbox;
    let last = f.last_uid.unwrap_or(0);
    if caps.condstore
        && let (Some(old), Some(high)) = (f.modseq, mb.highest_modseq)
    {
        if high as i64 > old {
            let updates: Vec<_> = fetch(
                s,
                &format!("UID FETCH 1:{last} (UID FLAGS) (CHANGEDSINCE {old})"),
            )
            .await?
            .into_iter()
            .filter_map(|it| Some((ext_id(&f.ext_id, validity, it.uid?), it.flags)))
            .collect();
            report.flag_updates += updates.len();
            sink.flags_batch(updates).await?;
        }
    } else if !caps.condstore
        && f.flags_checked.is_none_or(|t| now - t >= FLAG_RECHECK_MS)
        && mb.exists > 0
    {
        let from = mb.exists.saturating_sub(FLAG_RECHECK_COUNT - 1).max(1);
        let updates: Vec<_> = fetch(s, &format!("FETCH {from}:* (UID FLAGS)"))
            .await?
            .into_iter()
            .filter_map(|it| Some((ext_id(&f.ext_id, validity, it.uid?), it.flags)))
            .collect();
        report.flag_updates += updates.len();
        sink.flags_batch(updates).await?;
        f.flags_checked = Some(now);
    }
    Ok(())
}
