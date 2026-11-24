//! Structured causes for a turn or job ended by its Node.

use steve_types::{Error, Param, ReasonCode, Retry, devices::NodeStopReason};

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
    /// A turn on a harness without declared native compaction reached its context budget.
    ContextBudget,
}

/// Maximum single NDJSON message size accepted from a harness adapter on stdio.
pub const HARNESS_MAX_FRAME_BYTES: usize = 16 << 20;

const STALLED_HEAD: &str = "the node took none of the harness's output for ";
const STALLED_TAIL: &str = " s (its journal or disk is stuck)";

impl NodeStop {
    /// The `safe_detail` it goes over the wire with (the words Nodes have always used).
    pub fn detail(self) -> String {
        use NodeStop::*;
        let fixed = match self {
            Deadline => "past its deadline",
            JobDeadline => "past the job's deadline",
            HubUnreachable => "the Hub was unreachable past the lease",
            OutputLimit => "the output limit was reached",
            FrameTooLarge => "the harness sent a message larger than 16 MiB",
            IngressStalled { secs } => return format!("{STALLED_HEAD}{secs}{STALLED_TAIL}"),
            JournalBudget => "journal data budget reached",
            JournalFails => "the node's journal fails",
            NotRecorded => "the node could not record the attempt",
            HarnessGone => "the harness is gone",
            AdapterExited => "adapter exited",
            StoppedByNode => "stopped by the node",
            HarnessIdle => "the harness said nothing for too long",
            ResearchBudget => "research budget reached",
            ContextBudget => "turn context budget reached",
        };
        fixed.to_string()
    }

    pub fn error(self) -> Error {
        let detail = self.detail();
        let error = match self {
            Self::Deadline => NodeStopReason::deadline(detail),
            Self::JobDeadline => NodeStopReason::job_deadline(detail),
            Self::HubUnreachable => NodeStopReason::hub_unreachable(detail),
            Self::OutputLimit => NodeStopReason::output_limit(detail),
            Self::FrameTooLarge => NodeStopReason::frame_too_large(detail),
            Self::IngressStalled { secs } => {
                NodeStopReason::ingress_stalled(i64::try_from(secs).unwrap_or(i64::MAX), detail)
            }
            Self::JournalBudget => NodeStopReason::journal_budget(detail),
            Self::JournalFails => NodeStopReason::journal_fails(detail),
            Self::NotRecorded => NodeStopReason::not_recorded(detail),
            Self::HarnessGone => NodeStopReason::harness_gone(detail),
            Self::AdapterExited => NodeStopReason::adapter_exited(detail),
            Self::StoppedByNode => NodeStopReason::stopped_by_node(detail),
            Self::HarnessIdle => NodeStopReason::harness_idle(detail),
            Self::ResearchBudget => NodeStopReason::research_budget(detail),
            Self::ContextBudget => NodeStopReason::context_budget(detail),
        };
        error.retry(Retry::Never)
    }

    pub fn of(e: &Error) -> Option<Self> {
        let reason = e.reason.as_ref()?.known::<NodeStopReason>()?;
        if reason.def().code != e.code {
            return None;
        }
        Some(match reason {
            NodeStopReason::Deadline => Self::Deadline,
            NodeStopReason::JobDeadline => Self::JobDeadline,
            NodeStopReason::HubUnreachable => Self::HubUnreachable,
            NodeStopReason::OutputLimit => Self::OutputLimit,
            NodeStopReason::FrameTooLarge => Self::FrameTooLarge,
            NodeStopReason::IngressStalled => match e.params.0.get("secs")? {
                Param::Int(secs) => Self::IngressStalled {
                    secs: u64::try_from(*secs).ok()?,
                },
                Param::Str(_) => return None,
            },
            NodeStopReason::JournalBudget => Self::JournalBudget,
            NodeStopReason::JournalFails => Self::JournalFails,
            NodeStopReason::NotRecorded => Self::NotRecorded,
            NodeStopReason::HarnessGone => Self::HarnessGone,
            NodeStopReason::AdapterExited => Self::AdapterExited,
            NodeStopReason::StoppedByNode => Self::StoppedByNode,
            NodeStopReason::HarnessIdle => Self::HarnessIdle,
            NodeStopReason::ResearchBudget => Self::ResearchBudget,
            NodeStopReason::ContextBudget => Self::ContextBudget,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_types::Code;

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
            NodeStop::ContextBudget,
        ];
        for s in all {
            assert_eq!(NodeStop::of(&s.error()), Some(s), "{s:?}");
            assert_eq!(s.error().retry, Retry::Never);
            assert_eq!(NodeStop::of(&Error::new(s.error().code, s.detail())), None);
        }
        assert_eq!(NodeStop::of(&Error::new(Code::Internal, "boom")), None);
    }
}
