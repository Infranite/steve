//! Descriptions of effects only. The ledger driver will execute them after a guarded transition.
use super::machine::{Input, State, TABLE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    PersistState,
    SendSameOperation,
    RenewUnsentEnvelope,
    ReleaseLease,
    ArmDue,
    Reconcile,
    NotifySettled,
}

/// A triple not present in the table has no effects. Guards and CAS precede execution.
pub fn effects(from: State, on: Input, to: State) -> Vec<Effect> {
    if TABLE.step(from, on) != Ok(to) {
        return vec![];
    }
    if on == Input::RenewUnsent {
        return vec![Effect::RenewUnsentEnvelope];
    }
    if matches!(
        on,
        Input::Dispatch | Input::RetryDue | Input::RetryImmediate
    ) {
        return vec![Effect::PersistState, Effect::SendSameOperation];
    }
    if from == to {
        return vec![];
    }
    let mut effects = vec![Effect::PersistState];
    if from == State::Sending {
        effects.push(Effect::ReleaseLease);
    }
    if to == State::WaitingRetry {
        effects.push(Effect::ArmDue);
    }
    if to == State::Unknown {
        effects.push(Effect::Reconcile);
    }
    if TABLE.terminal.iter().any(|state| *state == to) {
        effects.push(Effect::NotifySettled);
    }
    effects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_sending_state_can_release_a_lease() {
        for row in TABLE.rows {
            let got = effects(row.from, row.on, row.to);
            if got.contains(&Effect::ReleaseLease) {
                assert_eq!(row.from, State::Sending);
            }
        }
        assert_eq!(
            effects(State::Unknown, Input::RecoveredOk, State::FinishedOk),
            vec![Effect::PersistState, Effect::NotifySettled]
        );
        assert_eq!(
            effects(State::Sending, Input::Later, State::WaitingRetry),
            vec![Effect::PersistState, Effect::ReleaseLease, Effect::ArmDue]
        );
    }

    #[test]
    fn a_lost_receipt_reconciles_and_a_late_terminal_observation_has_no_effects() {
        assert_eq!(
            effects(State::Sending, Input::LinkLost, State::Unknown),
            vec![
                Effect::PersistState,
                Effect::ReleaseLease,
                Effect::Reconcile,
            ]
        );
        assert!(effects(State::Unknown, Input::RetryDue, State::Sending).is_empty());
        assert_eq!(
            effects(State::Active, Input::Succeeded, State::FinishedOk),
            vec![Effect::PersistState, Effect::NotifySettled,]
        );
        assert!(effects(State::FinishedOk, Input::Failed, State::FinishedOk).is_empty());
    }

    #[test]
    fn only_guarded_command_rows_send_and_invalid_triples_do_nothing() {
        assert_eq!(
            effects(State::WaitingRetry, Input::RetryDue, State::Sending),
            vec![Effect::PersistState, Effect::SendSameOperation,]
        );
        assert!(effects(State::Sending, Input::LinkLost, State::Sending).is_empty());
        assert_eq!(
            effects(State::Planned, Input::RenewUnsent, State::Planned),
            vec![Effect::RenewUnsentEnvelope]
        );
        assert!(
            effects(State::Sending, Input::Accepted, State::Accepted)
                .contains(&Effect::ReleaseLease)
        );
        assert!(
            effects(State::Sending, Input::Later, State::WaitingRetry).contains(&Effect::ArmDue)
        );
    }
}
