#![cfg(feature = "machine")]
use steve_runtime::machine::{Class, Rejected, Row, Table, Violation, check};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Open,
    Locked,
    Removed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    Open,
    Close,
    Lock,
    Unlock,
    Remove,
    Wind,
}
const ROWS: [Row<State, Input>; 9] = [
    Row {
        from: State::Closed,
        on: Input::Open,
        to: State::Open,
    },
    Row {
        from: State::Open,
        on: Input::Close,
        to: State::Closed,
    },
    Row {
        from: State::Closed,
        on: Input::Lock,
        to: State::Locked,
    },
    Row {
        from: State::Locked,
        on: Input::Unlock,
        to: State::Closed,
    },
    Row {
        from: State::Closed,
        on: Input::Remove,
        to: State::Removed,
    },
    Row {
        from: State::Closed,
        on: Input::Wind,
        to: State::Closed,
    },
    Row {
        from: State::Open,
        on: Input::Wind,
        to: State::Closed,
    },
    Row {
        from: State::Locked,
        on: Input::Wind,
        to: State::Locked,
    },
    Row {
        from: State::Removed,
        on: Input::Wind,
        to: State::Removed,
    },
];
const DOOR: Table<State, Input> = Table {
    name: "door",
    states: &[State::Closed, State::Open, State::Locked, State::Removed],
    inputs: &[
        (Input::Open, Class::Command),
        (Input::Close, Class::Command),
        (Input::Lock, Class::Command),
        (Input::Unlock, Class::Command),
        (Input::Remove, Class::Command),
        (Input::Wind, Class::Observation),
    ],
    initial: &[State::Closed],
    terminal: &[State::Removed],
    rows: &ROWS,
};
const fn changed(index: usize, row: Row<State, Input>) -> [Row<State, Input>; 9] {
    let mut rows = ROWS;
    rows[index] = row;
    rows
}

#[test]
fn rows_and_the_full_matrix_are_the_domain_test_template() {
    assert_eq!(check::table(&DOOR), []);
    // Wave-4 callers copy this shape: for every row, create an entity in `from`, feed `on`,
    // and assert `to` plus the domain work unit's contents. This example has no side effects.
    for row in DOOR.rows {
        assert_eq!(DOOR.step(row.from, row.on), Ok(row.to));
    }
    // Then cover the whole state × input matrix, including the commands absent from the table.
    // Observations are facts: they must always have a row, even after the entity has ended.
    for &state in DOOR.states {
        for &(input, class) in DOOR.inputs {
            let row = DOOR.rows.iter().find(|r| r.from == state && r.on == input);
            match row {
                Some(row) => assert_eq!(DOOR.step(state, input), Ok(row.to)),
                None => {
                    assert_eq!(
                        class,
                        Class::Command,
                        "unhandled observation: {state:?}/{input:?}"
                    );
                    assert_eq!(
                        DOOR.step(state, input),
                        Err(Rejected {
                            table: "door",
                            from: state,
                            on: input
                        })
                    );
                }
            }
        }
    }
    assert_eq!(DOOR.step(State::Open, Input::Wind), Ok(State::Closed));
    assert_eq!(DOOR.step(State::Removed, Input::Wind), Ok(State::Removed));
}

#[test]
fn duplicate_pairs_are_rejected_even_when_the_targets_agree() {
    const SAME: Table<State, Input> = Table {
        rows: &changed(1, ROWS[0]),
        ..DOOR
    };
    const DIFFERENT: Table<State, Input> = Table {
        rows: &changed(
            1,
            Row {
                to: State::Locked,
                ..ROWS[0]
            },
        ),
        ..DOOR
    };
    for table in [SAME, DIFFERENT] {
        assert_eq!(
            check::table(&table),
            [Violation::DuplicateTransition {
                first: 0,
                second: 1
            }]
        );
    }
}

#[test]
fn terminal_states_allow_only_observation_self_loops() {
    const COMMAND: Table<State, Input> = Table {
        rows: &changed(
            8,
            Row {
                on: Input::Remove,
                ..ROWS[8]
            },
        ),
        ..DOOR
    };
    const EXIT: Table<State, Input> = Table {
        rows: &changed(
            8,
            Row {
                to: State::Closed,
                ..ROWS[8]
            },
        ),
        ..DOOR
    };
    for table in [COMMAND, EXIT] {
        assert!(check::table(&table).contains(&Violation::TerminalTransition { row: 8 }));
    }
}

#[test]
fn every_state_must_be_reachable_from_an_initial_state() {
    const BAD: Table<State, Input> = Table {
        rows: &changed(
            0,
            Row {
                to: State::Closed,
                ..ROWS[0]
            },
        ),
        ..DOOR
    };
    assert_eq!(check::table(&BAD), [Violation::Unreachable { state: 1 }]);
}

#[test]
fn a_cycle_without_a_terminal_path_is_rejected() {
    const BAD: Table<State, Input> = Table {
        initial: &[State::Closed, State::Removed],
        rows: &changed(
            4,
            Row {
                to: State::Closed,
                ..ROWS[4]
            },
        ),
        ..DOOR
    };
    assert_eq!(
        check::table(&BAD),
        [
            Violation::NoTerminalPath { state: 0 },
            Violation::NoTerminalPath { state: 1 },
            Violation::NoTerminalPath { state: 2 },
        ]
    );
}

#[test]
fn missing_observations_are_reported_and_step_still_returns_rejected() {
    const BAD: Table<State, Input> = Table {
        rows: &changed(
            8,
            Row {
                from: State::Closed,
                on: Input::Close,
                to: State::Closed,
            },
        ),
        ..DOOR
    };
    assert_eq!(
        check::table(&BAD),
        [Violation::MissingObservation { state: 3, input: 5 }]
    );
    assert_eq!(
        BAD.step(State::Removed, Input::Wind),
        Err(Rejected {
            table: "door",
            from: State::Removed,
            on: Input::Wind
        })
    );
}

#[test]
fn row_states_and_inputs_must_be_declared() {
    const STATES: Table<State, Input> = Table {
        states: &[State::Closed, State::Open, State::Removed],
        ..DOOR
    };
    let errors = check::table(&STATES);
    assert!(errors.contains(&Violation::UnknownTo { row: 2 }));
    assert!(errors.contains(&Violation::UnknownFrom { row: 3 }));
    const INPUTS: Table<State, Input> = Table {
        inputs: &[],
        ..DOOR
    };
    assert!(check::table(&INPUTS).contains(&Violation::UnknownInput { row: 0 }));
}

#[test]
fn input_declarations_cannot_repeat_or_disagree_on_class() {
    const SAME: Table<State, Input> = Table {
        inputs: &[
            (Input::Wind, Class::Observation),
            (Input::Wind, Class::Observation),
        ],
        ..DOOR
    };
    const DIFFERENT: Table<State, Input> = Table {
        inputs: &[
            (Input::Wind, Class::Observation),
            (Input::Wind, Class::Command),
        ],
        ..DOOR
    };
    for table in [SAME, DIFFERENT] {
        assert!(check::table(&table).contains(&Violation::DuplicateInput {
            first: 0,
            second: 1
        }));
    }
}

#[test]
fn state_and_root_declarations_are_checked() {
    const DUPLICATE: Table<State, Input> = Table {
        states: &[State::Closed, State::Closed],
        ..DOOR
    };
    assert!(
        check::table(&DUPLICATE).contains(&Violation::DuplicateState {
            first: 0,
            second: 1
        })
    );
    const INITIAL: Table<State, Input> = Table {
        states: &[State::Removed],
        ..DOOR
    };
    assert!(check::table(&INITIAL).contains(&Violation::UnknownInitial { initial: 0 }));
    const TERMINAL: Table<State, Input> = Table {
        states: &[State::Closed],
        ..DOOR
    };
    assert!(check::table(&TERMINAL).contains(&Violation::UnknownTerminal { terminal: 0 }));
}

#[test]
fn undeclared_vertices_do_not_create_paths_between_declared_states() {
    const BAD: Table<u8, u8> = Table {
        name: "undeclared bridge",
        states: &[0, 2],
        inputs: &[(0, Class::Command)],
        initial: &[0],
        terminal: &[2],
        rows: &[
            Row {
                from: 0,
                on: 0,
                to: 1,
            },
            Row {
                from: 1,
                on: 0,
                to: 2,
            },
        ],
    };
    let errors = check::table(&BAD);
    assert!(errors.contains(&Violation::Unreachable { state: 1 }));
    assert!(errors.contains(&Violation::NoTerminalPath { state: 0 }));
}

#[test]
fn multiple_initial_and_terminal_states_can_describe_disconnected_components() {
    const COMPONENTS: Table<u8, u8> = Table {
        name: "components",
        states: &[0, 1, 2, 3],
        inputs: &[(0, Class::Command), (1, Class::Observation)],
        initial: &[0, 2],
        terminal: &[1, 3],
        rows: &[
            Row {
                from: 0,
                on: 0,
                to: 1,
            },
            Row {
                from: 2,
                on: 0,
                to: 3,
            },
            Row {
                from: 0,
                on: 1,
                to: 0,
            },
            Row {
                from: 1,
                on: 1,
                to: 1,
            },
            Row {
                from: 2,
                on: 1,
                to: 2,
            },
            Row {
                from: 3,
                on: 1,
                to: 3,
            },
        ],
    };
    assert_eq!(check::table(&COMPONENTS), []);
}
