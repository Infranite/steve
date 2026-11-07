//! Why a Node itself ended a turn or a job: deadline, output limit, a stuck ingress and the like.
//!
//! The wire carries the cause only as an [`ApiError`]'s code and `safe_detail`, so a Node of any
//! version tells it in the same words. The Node builds these errors with [`NodeStop::error`] and the
//! Hub reads them back with [`NodeStop::of`] to say them in the user's language; a detail it does not
//! recognize (an older or newer Node, a harness's own words) is shown as it came.

use crate::common::{ApiError, ErrorCode};

/// The ends a Node decides on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStop {
    /// The run's hard time limit.
    Deadline,
    /// A job's hard time limit.
    JobDeadline,
    /// The Hub stayed unreachable past the run's lease.
    HubUnreachable,
    /// More output than the attempt's budget.
    OutputLimit,
    /// One harness message over the frame limit.
    FrameTooLarge,
    /// The Node took none of the harness's output for this many seconds.
    IngressStalled { secs: u64 },
    /// The Node's journal is over its data budget.
    JournalBudget,
    /// The Node's journal fails to write.
    JournalFails,
    /// The Node could not record the attempt's output (its journal refused the write).
    NotRecorded,
    /// The harness process is gone.
    HarnessGone,
    /// The adapter exited while starting.
    AdapterExited,
    /// The Node stopped it (its own shutdown or recovery).
    StoppedByNode,
    /// The harness said nothing for the job's idle limit.
    HarnessIdle,
    /// A research job used up its turns or tool calls.
    ResearchBudget,
}

const STALLED_HEAD: &str = "the node took none of the harness's output for ";
const STALLED_TAIL: &str = " s (its journal or disk is stuck)";

impl NodeStop {
    pub fn code(self) -> ErrorCode {
        use NodeStop::*;
        match self {
            Deadline | JobDeadline | HarnessIdle => ErrorCode::Expired,
            HubUnreachable | HarnessGone | AdapterExited => ErrorCode::Offline,
            OutputLimit | FrameTooLarge | JournalBudget | ResearchBudget => ErrorCode::Exhausted,
            IngressStalled { .. } => ErrorCode::Busy,
            JournalFails | NotRecorded | StoppedByNode => ErrorCode::Internal,
        }
    }

    /// The `safe_detail` it goes over the wire with (the words Nodes have always used).
    pub fn detail(self) -> String {
        use NodeStop::*;
        let fixed = match self {
            Deadline => "past its deadline",
            JobDeadline => "past the job's deadline",
            HubUnreachable => "the Hub was unreachable past the lease",
            OutputLimit => "the output limit was reached",
            FrameTooLarge => "the harness sent a message larger than 1 MiB",
            IngressStalled { secs } => return format!("{STALLED_HEAD}{secs}{STALLED_TAIL}"),
            JournalBudget => "journal data budget reached",
            JournalFails => "the node's journal fails",
            NotRecorded => "the node could not record the attempt",
            HarnessGone => "the harness is gone",
            AdapterExited => "adapter exited",
            StoppedByNode => "stopped by the node",
            HarnessIdle => "the harness said nothing for too long",
            ResearchBudget => "research budget reached",
        };
        fixed.to_string()
    }

    pub fn error(self) -> ApiError {
        ApiError::new(self.code(), self.detail())
    }

    /// The cause behind `e`, when a Node said one of these: same code, same words.
    pub fn of(e: &ApiError) -> Option<NodeStop> {
        use NodeStop::*;
        let d = e.safe_detail.as_str();
        let stop = if let Some(secs) = d
            .strip_prefix(STALLED_HEAD)
            .and_then(|r| r.strip_suffix(STALLED_TAIL))
        {
            IngressStalled {
                secs: secs.parse().ok()?,
            }
        } else {
            [
                Deadline,
                JobDeadline,
                HubUnreachable,
                OutputLimit,
                FrameTooLarge,
                JournalBudget,
                JournalFails,
                NotRecorded,
                HarnessGone,
                AdapterExited,
                StoppedByNode,
                HarnessIdle,
                ResearchBudget,
            ]
            .into_iter()
            .find(|s| s.detail() == d)?
        };
        (stop.code() == e.code).then_some(stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cause_reads_back_and_only_with_its_code() {
        let all = [
            NodeStop::Deadline,
            NodeStop::JobDeadline,
            NodeStop::HubUnreachable,
            NodeStop::OutputLimit,
            NodeStop::FrameTooLarge,
            NodeStop::IngressStalled { secs: 120 },
            NodeStop::JournalBudget,
            NodeStop::JournalFails,
            NodeStop::NotRecorded,
            NodeStop::HarnessGone,
            NodeStop::AdapterExited,
            NodeStop::StoppedByNode,
            NodeStop::HarnessIdle,
            NodeStop::ResearchBudget,
        ];
        for s in all {
            assert_eq!(NodeStop::of(&s.error()), Some(s), "{s:?}");
            let other = if s.code() == ErrorCode::Invalid {
                ErrorCode::Internal
            } else {
                ErrorCode::Invalid
            };
            assert_eq!(NodeStop::of(&ApiError::new(other, s.detail())), None);
        }
        // the words Nodes sent before this type existed
        assert_eq!(
            NodeStop::of(&ApiError::new(ErrorCode::Expired, "past its deadline")),
            Some(NodeStop::Deadline)
        );
        assert_eq!(
            NodeStop::of(&ApiError::new(
                ErrorCode::Busy,
                "the node took none of the harness's output for 60 s (its journal or disk is stuck)"
            )),
            Some(NodeStop::IngressStalled { secs: 60 })
        );
        assert_eq!(
            NodeStop::of(&ApiError::new(ErrorCode::Internal, "boom")),
            None
        );
    }
}
