//! Back to a trigger word while its item runs: an item handed off from your trigger word (an
//! answer said as you, [`crate::command_skill`]) reports to the message it came from without the
//! agent having to remember to. A failure is said at once (the same reason at most every
//! [`ERROR_REPEAT_MS`], as its runs are retried); what the item is doing now is said when it
//! changed and [`PROGRESS_EVERY_MS`] passed since the last report. Its end says nothing: the
//! message's reaction turns to done (`react_item_ended`). All of it goes out as you, `[AI] `-marked, where the trigger's
//! settings say; the reaction stays until the end. What was said last is kept per item in
//! `settings` (`trigger.report.<item>`).

use std::sync::Arc;
use std::time::Duration;

use steve_store::Read;
use steve_store::channel_trigger_report::{self as report, History as Said, OpenItem as Open};

use crate::deliver::{Auth, OutRequest, Purpose};
use crate::outbound::Outbox;
use crate::{ChannelResult, Channels};

/// How often open items are looked at.
pub const REPORT_EVERY: Duration = Duration::from_secs(60);
/// The least time between two progress reports of one item.
pub const PROGRESS_EVERY_MS: i64 = 30 * 60_000;
/// The least time before the same failure is said again.
pub const ERROR_REPEAT_MS: i64 = 30 * 60_000;

/// The newest failure after `after`, as the session explained it.
fn failure(
    r: &mut impl Read,
    item: &str,
    after: i64,
) -> steve_store::Result<Option<(i64, String)>> {
    let Some(failure) = steve_store::diagnostics::latest_item_failure(r, item, after)? else {
        return Ok(None);
    };
    let reason = match failure.session {
        Some(session) => steve_store::session_repo::latest_system_text(r, &session)?,
        None => None,
    };
    let reason = reason
        .map(|r| r.trim().trim_end_matches('。').to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| format!("执行出错（{}）", failure.kind));
    Ok(Some((failure.seq, reason)))
}

/// What to say of `o` now, given what was said last; the new record either way.
fn next(
    o: &Open,
    last: Option<Said>,
    fail: Option<(i64, String)>,
    now: i64,
) -> (Option<String>, Said) {
    // first look: what it is doing now is the starting point, nothing is said of it yet
    let mut s = last.clone().unwrap_or(Said {
        now: o.now.clone(),
        at: now,
        ..Said::default()
    });
    if let Some((seq, reason)) = fail {
        s.diag_seq = seq;
        let repeat = reason == s.error && now - s.error_at < ERROR_REPEAT_MS;
        if !repeat {
            s.error = reason.clone();
            s.error_at = now;
            return (
                Some(format!("{} 执行遇到问题：{reason}，正在重试。", o.item)),
                s,
            );
        }
    }
    let changed = !o.now.trim().is_empty() && o.now != s.now;
    if last.is_some() && changed && now - s.at >= PROGRESS_EVERY_MS {
        s.now = o.now.clone();
        s.at = now;
        return (Some(format!("{} 进展：{}", o.item, o.now.trim())), s);
    }
    (None, s)
}

/// One look at every open item handed off from a trigger word.
pub async fn sweep(ch: &Channels, outbox: &Outbox) -> ChannelResult<usize> {
    let now = ch.now();
    let open = ch.store().read(|r| report::open_items(r))?;
    let mut n = 0;
    for o in open {
        let say = ch.store().write(|w| -> ChannelResult<_> {
            let last = report::history(w, &o.ws, &o.item)?;
            let fail = failure(w, &o.item, last.as_ref().map_or(0, |l| l.diag_seq))?;
            let (say, state) = next(&o, last, fail, now);
            report::keep(w, &o.ws, &o.item, &state, now)?;
            Ok(say)
        })?;
        let Some(text) = say else { continue };
        let plan = ch.store().read(|r| {
            ch.store()
                .reader(|c| crate::command_skill::trigger_plan(c, r, &o.ws, &o.message))
        })?;
        let Some(plan) = plan else { continue };
        let req = OutRequest {
            target: crate::command_skill::target(ch, &plan)?,
            text,
            // not the end: the reaction stays
            purpose: Purpose::Source,
            approve: false,
            generation: None,
            auth: Auth::Trigger {
                source: plan.message_id.clone(),
            },
            operation_id: None,
        };
        match outbox.deliver_checked(ch, &req).await {
            Ok((d, _)) => {
                ch.after_trigger_send(&o.ws, &plan.connection_id, &d);
                n += 1;
            }
            Err(e) => tracing::warn!(item = o.item, "trigger report not sent: {e}"),
        }
    }
    Ok(n)
}

/// The job: [`sweep`] every [`REPORT_EVERY`].
pub fn run(ch: Channels, outbox: Arc<Outbox>) -> steve_runtime::Task {
    steve_runtime::every_after(
        "channels.trigger_report",
        ch.modules().clock.clone(),
        REPORT_EVERY,
        steve_runtime::FirstRun::Immediate,
        move || {
            let (ch, outbox) = (ch.clone(), outbox.clone());
            async move {
                if let Err(e) = sweep(&ch, &outbox).await {
                    tracing::warn!("trigger reports: {e}");
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(now: &str) -> Open {
        Open {
            ws: "w".into(),
            item: "ST-1".into(),
            message: "cm".into(),
            now: now.into(),
        }
    }

    #[test]
    fn a_failure_is_said_at_once_and_not_again_while_it_repeats() {
        let fail = Some((
            7,
            "Node 没能打开这个会话（Authentication required）".to_string(),
        ));
        let (say, s) = next(&open("开始"), None, fail.clone(), 1_000);
        assert_eq!(
            say.as_deref(),
            Some("ST-1 执行遇到问题：Node 没能打开这个会话（Authentication required），正在重试。")
        );
        // retried: the same reason again soon is not said
        let (say, s) = next(
            &open("开始"),
            Some(s),
            Some((9, fail.clone().unwrap().1)),
            60_000,
        );
        assert_eq!((say, s.diag_seq), (None, 9));
        // much later, or another reason: said
        let (say, _) = next(
            &open("开始"),
            Some(s.clone()),
            Some((10, fail.unwrap().1)),
            1_000 + ERROR_REPEAT_MS,
        );
        assert!(say.is_some());
        let (say, _) = next(
            &open("开始"),
            Some(s),
            Some((11, "别的问题".into())),
            70_000,
        );
        assert!(say.unwrap().contains("别的问题"));
    }

    #[test]
    fn progress_is_said_when_it_changed_and_enough_time_passed() {
        let (say, s) = next(&open("开始"), None, None, 0);
        assert_eq!(say, None, "the first look is the starting point");
        let (say, s) = next(&open("在查日志"), Some(s), None, PROGRESS_EVERY_MS - 1);
        assert_eq!(say, None, "too soon");
        let (say, s) = next(&open("在查日志"), Some(s), None, PROGRESS_EVERY_MS);
        assert_eq!(say.as_deref(), Some("ST-1 进展：在查日志"));
        let (say, _) = next(&open("在查日志"), Some(s), None, 3 * PROGRESS_EVERY_MS);
        assert_eq!(say, None, "nothing new");
    }
}
