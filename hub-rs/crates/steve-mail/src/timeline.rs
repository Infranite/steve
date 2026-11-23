//! Mail sent to or received from a contact, including contacts merged into it.

use steve_modules::ApiResult;
use steve_modules::contacts::{TimelineEntry, TimelineSource};
use steve_store::{Store, base::Viewer, mail};

pub struct MailTimeline;

impl TimelineSource for MailTimeline {
    fn entries(
        &self,
        store: &Store,
        viewer: &Viewer,
        party: &str,
        before: Option<i64>,
        limit: usize,
    ) -> ApiResult<Vec<TimelineEntry>> {
        let rows = store.read(|r| mail::party_timeline(r, viewer, party, before, limit as u64))?;
        Ok(rows
            .into_iter()
            .map(|(thread, ts, direction, subject)| {
                let subject = if subject.is_empty() {
                    steve_i18n::t!("mail.subject.none")
                } else {
                    subject
                };
                TimelineEntry {
                    ts,
                    kind: "mail".into(),
                    text: if direction == "out" {
                        steve_i18n::t!("mail.timeline.sent", subject = subject)
                    } else {
                        steve_i18n::t!("mail.timeline.received", subject = subject)
                    },
                    href: format!("#/mail/t/{thread}"),
                }
            })
            .collect())
    }
}
