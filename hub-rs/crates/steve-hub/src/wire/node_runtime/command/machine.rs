//! Inputs have already been classified by policy; no payload or error text enters the table.
use steve_runtime::machine::{Class, Row, Table};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Planned,
    Sending,
    Accepted,
    Active,
    WaitingRetry,
    Unknown,
    FinishedOk,
    FinishedFailed,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Dispatch,
    RetryDue,
    RetryImmediate,
    RenewUnsent,
    Accepted,
    Running,
    Succeeded,
    Failed,
    Never,
    Later,
    OutcomeUnknown,
    LinkLost,
    NodeForgot,
    WindowExpired,
    ReconciledAbsent,
    RecoveredOk,
    RecoveredFailed,
    Abandoned,
}

// Named rows also exercise the actual runtime Table::step API, one test per transition.
macro_rules! rows {
    ($($test:ident: $from:ident + $on:ident => $to:ident, $why:literal;)*) => {
        const ROWS: &[Row<State, Input>] = &[$(Row {
            from: State::$from, on: Input::$on, to: State::$to, why: $why,
        }),*];
        #[cfg(test)]
        mod rows {
            use super::*;
            $(#[test] fn $test() {
                assert_eq!(TABLE.step(State::$from, Input::$on), Ok(State::$to));
            })*
        }
    };
}

rows! {
    dispatch: Planned + Dispatch => Sending, "";
    retry_due: WaitingRetry + RetryDue => Sending, "policy checked the deadline and due time";
    retry_immediate: Sending + RetryImmediate => Sending, "policy checked the immediate retry budget";
    renew_unsent: Planned + RenewUnsent => Planned, "caller proved that the envelope never left the Hub";
    planned_accepted: Planned + Accepted => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_running: Planned + Running => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_succeeded: Planned + Succeeded => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_failed: Planned + Failed => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_never: Planned + Never => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_later: Planned + Later => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_outcome_unknown: Planned + OutcomeUnknown => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_link_lost: Planned + LinkLost => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_node_forgot: Planned + NodeForgot => Planned, "no command was sent; unrelated observations cannot settle it";
    planned_window_expired: Planned + WindowExpired => Rejected, "";
    sending_accepted: Sending + Accepted => Accepted, "";
    sending_running: Sending + Running => Active, "";
    sending_succeeded: Sending + Succeeded => FinishedOk, "";
    sending_failed: Sending + Failed => FinishedFailed, "";
    sending_never: Sending + Never => Rejected, "";
    sending_later: Sending + Later => WaitingRetry, "";
    sending_outcome_unknown: Sending + OutcomeUnknown => Unknown, "";
    sending_link_lost: Sending + LinkLost => Unknown, "";
    sending_node_forgot: Sending + NodeForgot => Unknown, "";
    sending_window_expired: Sending + WindowExpired => Unknown, "";
    accepted_accepted: Accepted + Accepted => Accepted, "late receipts and envelope expiry cannot regress an accepted execution";
    accepted_running: Accepted + Running => Active, "";
    accepted_succeeded: Accepted + Succeeded => FinishedOk, "";
    accepted_failed: Accepted + Failed => FinishedFailed, "";
    accepted_never: Accepted + Never => Accepted, "late receipts and envelope expiry cannot regress an accepted execution";
    accepted_later: Accepted + Later => Accepted, "late receipts and envelope expiry cannot regress an accepted execution";
    accepted_outcome_unknown: Accepted + OutcomeUnknown => Unknown, "";
    accepted_link_lost: Accepted + LinkLost => Unknown, "";
    accepted_node_forgot: Accepted + NodeForgot => Unknown, "";
    accepted_window_expired: Accepted + WindowExpired => Accepted, "late receipts and envelope expiry cannot regress an accepted execution";
    active_accepted: Active + Accepted => Active, "late receipts and envelope expiry cannot regress an accepted execution";
    active_running: Active + Running => Active, "late receipts and envelope expiry cannot regress an accepted execution";
    active_succeeded: Active + Succeeded => FinishedOk, "";
    active_failed: Active + Failed => FinishedFailed, "";
    active_never: Active + Never => Active, "late receipts and envelope expiry cannot regress an accepted execution";
    active_later: Active + Later => Active, "late receipts and envelope expiry cannot regress an accepted execution";
    active_outcome_unknown: Active + OutcomeUnknown => Unknown, "";
    active_link_lost: Active + LinkLost => Unknown, "";
    active_node_forgot: Active + NodeForgot => Unknown, "";
    active_window_expired: Active + WindowExpired => Active, "late receipts and envelope expiry cannot regress an accepted execution";
    waiting_retry_accepted: WaitingRetry + Accepted => Accepted, "";
    waiting_retry_running: WaitingRetry + Running => Active, "";
    waiting_retry_succeeded: WaitingRetry + Succeeded => FinishedOk, "";
    waiting_retry_failed: WaitingRetry + Failed => FinishedFailed, "";
    waiting_retry_never: WaitingRetry + Never => Rejected, "";
    waiting_retry_later: WaitingRetry + Later => WaitingRetry, "a pre-accept refusal proved no execution; reconnect before the scheduled send";
    waiting_retry_outcome_unknown: WaitingRetry + OutcomeUnknown => Unknown, "";
    waiting_retry_link_lost: WaitingRetry + LinkLost => WaitingRetry, "a pre-accept refusal proved no execution; reconnect before the scheduled send";
    waiting_retry_node_forgot: WaitingRetry + NodeForgot => WaitingRetry, "a pre-accept refusal proved no execution; reconnect before the scheduled send";
    waiting_retry_window_expired: WaitingRetry + WindowExpired => Rejected, "";
    unknown_accepted: Unknown + Accepted => Accepted, "";
    unknown_running: Unknown + Running => Active, "";
    unknown_succeeded: Unknown + Succeeded => FinishedOk, "";
    unknown_failed: Unknown + Failed => FinishedFailed, "";
    unknown_never: Unknown + Never => Unknown, "uncertainty needs a positive reconciliation or a settled recover/abandon result";
    unknown_later: Unknown + Later => Unknown, "uncertainty needs a positive reconciliation or a settled recover/abandon result";
    unknown_outcome_unknown: Unknown + OutcomeUnknown => Unknown, "uncertainty needs a positive reconciliation or a settled recover/abandon result";
    unknown_link_lost: Unknown + LinkLost => Unknown, "uncertainty needs a positive reconciliation or a settled recover/abandon result";
    unknown_node_forgot: Unknown + NodeForgot => Unknown, "uncertainty needs a positive reconciliation or a settled recover/abandon result";
    unknown_window_expired: Unknown + WindowExpired => Unknown, "uncertainty needs a positive reconciliation or a settled recover/abandon result";
    finished_ok_accepted: FinishedOk + Accepted => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_running: FinishedOk + Running => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_succeeded: FinishedOk + Succeeded => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_failed: FinishedOk + Failed => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_never: FinishedOk + Never => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_later: FinishedOk + Later => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_outcome_unknown: FinishedOk + OutcomeUnknown => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_link_lost: FinishedOk + LinkLost => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_node_forgot: FinishedOk + NodeForgot => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_ok_window_expired: FinishedOk + WindowExpired => FinishedOk, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_accepted: FinishedFailed + Accepted => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_running: FinishedFailed + Running => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_succeeded: FinishedFailed + Succeeded => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_failed: FinishedFailed + Failed => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_never: FinishedFailed + Never => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_later: FinishedFailed + Later => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_outcome_unknown: FinishedFailed + OutcomeUnknown => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_link_lost: FinishedFailed + LinkLost => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_node_forgot: FinishedFailed + NodeForgot => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    finished_failed_window_expired: FinishedFailed + WindowExpired => FinishedFailed, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_accepted: Rejected + Accepted => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_running: Rejected + Running => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_succeeded: Rejected + Succeeded => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_failed: Rejected + Failed => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_never: Rejected + Never => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_later: Rejected + Later => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_outcome_unknown: Rejected + OutcomeUnknown => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_link_lost: Rejected + LinkLost => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_node_forgot: Rejected + NodeForgot => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    rejected_window_expired: Rejected + WindowExpired => Rejected, "terminal outcomes are monotonic; late observations cannot resurrect them";
    planned_reconciled_absent: Planned + ReconciledAbsent => Planned, "late or unrelated recovery facts cannot regress this command";
    planned_recovered_ok: Planned + RecoveredOk => Planned, "late or unrelated recovery facts cannot regress this command";
    planned_recovered_failed: Planned + RecoveredFailed => Planned, "late or unrelated recovery facts cannot regress this command";
    planned_abandoned: Planned + Abandoned => Planned, "late or unrelated recovery facts cannot regress this command";
    sending_reconciled_absent: Sending + ReconciledAbsent => Sending, "late or unrelated recovery facts cannot regress this command";
    sending_recovered_ok: Sending + RecoveredOk => Sending, "late or unrelated recovery facts cannot regress this command";
    sending_recovered_failed: Sending + RecoveredFailed => Sending, "late or unrelated recovery facts cannot regress this command";
    sending_abandoned: Sending + Abandoned => Sending, "late or unrelated recovery facts cannot regress this command";
    accepted_reconciled_absent: Accepted + ReconciledAbsent => Accepted, "late or unrelated recovery facts cannot regress this command";
    accepted_recovered_ok: Accepted + RecoveredOk => Accepted, "late or unrelated recovery facts cannot regress this command";
    accepted_recovered_failed: Accepted + RecoveredFailed => Accepted, "late or unrelated recovery facts cannot regress this command";
    accepted_abandoned: Accepted + Abandoned => Accepted, "late or unrelated recovery facts cannot regress this command";
    active_reconciled_absent: Active + ReconciledAbsent => Active, "late or unrelated recovery facts cannot regress this command";
    active_recovered_ok: Active + RecoveredOk => Active, "late or unrelated recovery facts cannot regress this command";
    active_recovered_failed: Active + RecoveredFailed => Active, "late or unrelated recovery facts cannot regress this command";
    active_abandoned: Active + Abandoned => Active, "late or unrelated recovery facts cannot regress this command";
    waiting_retry_reconciled_absent: WaitingRetry + ReconciledAbsent => WaitingRetry, "late or unrelated recovery facts cannot regress this command";
    waiting_retry_recovered_ok: WaitingRetry + RecoveredOk => WaitingRetry, "late or unrelated recovery facts cannot regress this command";
    waiting_retry_recovered_failed: WaitingRetry + RecoveredFailed => WaitingRetry, "late or unrelated recovery facts cannot regress this command";
    waiting_retry_abandoned: WaitingRetry + Abandoned => WaitingRetry, "late or unrelated recovery facts cannot regress this command";
    unknown_reconciled_absent: Unknown + ReconciledAbsent => FinishedFailed, "";
    unknown_recovered_ok: Unknown + RecoveredOk => FinishedOk, "";
    unknown_recovered_failed: Unknown + RecoveredFailed => FinishedFailed, "";
    unknown_abandoned: Unknown + Abandoned => FinishedFailed, "";
    finished_ok_reconciled_absent: FinishedOk + ReconciledAbsent => FinishedOk, "late or unrelated recovery facts cannot regress this command";
    finished_ok_recovered_ok: FinishedOk + RecoveredOk => FinishedOk, "late or unrelated recovery facts cannot regress this command";
    finished_ok_recovered_failed: FinishedOk + RecoveredFailed => FinishedOk, "late or unrelated recovery facts cannot regress this command";
    finished_ok_abandoned: FinishedOk + Abandoned => FinishedOk, "late or unrelated recovery facts cannot regress this command";
    finished_failed_reconciled_absent: FinishedFailed + ReconciledAbsent => FinishedFailed, "late or unrelated recovery facts cannot regress this command";
    finished_failed_recovered_ok: FinishedFailed + RecoveredOk => FinishedFailed, "late or unrelated recovery facts cannot regress this command";
    finished_failed_recovered_failed: FinishedFailed + RecoveredFailed => FinishedFailed, "late or unrelated recovery facts cannot regress this command";
    finished_failed_abandoned: FinishedFailed + Abandoned => FinishedFailed, "late or unrelated recovery facts cannot regress this command";
    rejected_reconciled_absent: Rejected + ReconciledAbsent => Rejected, "late or unrelated recovery facts cannot regress this command";
    rejected_recovered_ok: Rejected + RecoveredOk => Rejected, "late or unrelated recovery facts cannot regress this command";
    rejected_recovered_failed: Rejected + RecoveredFailed => Rejected, "late or unrelated recovery facts cannot regress this command";
    rejected_abandoned: Rejected + Abandoned => Rejected, "late or unrelated recovery facts cannot regress this command";
}

pub const TABLE: Table<State, Input> = Table {
    name: "node.command",
    states: &[
        State::Planned,
        State::Sending,
        State::Accepted,
        State::Active,
        State::WaitingRetry,
        State::Unknown,
        State::FinishedOk,
        State::FinishedFailed,
        State::Rejected,
    ],
    inputs: &[
        (Input::Dispatch, Class::Command),
        (Input::RetryDue, Class::Command),
        (Input::RetryImmediate, Class::Command),
        (Input::RenewUnsent, Class::Command),
        (Input::Accepted, Class::Observation),
        (Input::Running, Class::Observation),
        (Input::Succeeded, Class::Observation),
        (Input::Failed, Class::Disruption),
        (Input::Never, Class::Disruption),
        (Input::Later, Class::Disruption),
        (Input::OutcomeUnknown, Class::Disruption),
        (Input::LinkLost, Class::Disruption),
        (Input::NodeForgot, Class::Disruption),
        (Input::WindowExpired, Class::Disruption),
        (Input::ReconciledAbsent, Class::Disruption),
        (Input::RecoveredOk, Class::Observation),
        (Input::RecoveredFailed, Class::Disruption),
        (Input::Abandoned, Class::Disruption),
    ],
    initial: &[State::Planned],
    transient: &[
        State::Planned,
        State::Sending,
        State::Accepted,
        State::Active,
        State::WaitingRetry,
        State::Unknown,
    ],
    terminal: &[State::FinishedOk, State::FinishedFailed, State::Rejected],
    rows: ROWS,
};

pub fn next(
    from: State,
    on: Input,
) -> Result<State, steve_runtime::machine::Rejected<State, Input>> {
    TABLE.step(from, on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_and_recover_or_abandon_settle_unknown_without_resending() {
        for (input, expected) in [
            (Input::ReconciledAbsent, State::FinishedFailed),
            (Input::RecoveredOk, State::FinishedOk),
            (Input::RecoveredFailed, State::FinishedFailed),
            (Input::Abandoned, State::FinishedFailed),
        ] {
            let unknown = next(
                next(State::Planned, Input::Dispatch).unwrap(),
                Input::LinkLost,
            )
            .unwrap();
            let settled = next(unknown, input).unwrap();
            assert_eq!(settled, expected);
            assert!(next(settled, Input::RetryDue).is_err());
            assert_eq!(next(settled, Input::LinkLost).unwrap(), settled);
        }
    }

    #[test]
    fn the_real_table_has_no_structural_violations() {
        assert_eq!(steve_runtime::machine::check::table(&TABLE), vec![]);
        assert_eq!(TABLE.rows.len(), 130);
    }

    #[test]
    fn uncertain_delivery_requires_reconciliation_never_a_blind_resend() {
        let sending = next(State::Planned, Input::Dispatch).unwrap();
        let unknown = next(sending, Input::LinkLost).unwrap();
        assert_eq!(unknown, State::Unknown);
        for command in [
            Input::Dispatch,
            Input::RetryDue,
            Input::RetryImmediate,
            Input::RenewUnsent,
        ] {
            assert!(next(unknown, command).is_err());
        }
        assert_eq!(next(unknown, Input::Running).unwrap(), State::Active);
        assert_eq!(
            next(State::Active, Input::Succeeded).unwrap(),
            State::FinishedOk
        );
    }

    #[test]
    fn all_observations_are_total_and_all_terminal_states_remain_terminal() {
        for from in TABLE.states {
            for (on, class) in TABLE.inputs {
                if *class != Class::Command {
                    let to = next(*from, *on).unwrap();
                    if TABLE.terminal.contains(from) {
                        assert_eq!(to, *from);
                    }
                } else if TABLE.terminal.contains(from) {
                    assert!(next(*from, *on).is_err());
                }
            }
        }
    }
}
