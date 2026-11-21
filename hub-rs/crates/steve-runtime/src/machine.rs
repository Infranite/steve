//! Static transition tables. Check a table's structure before using its pure `step` function.
//! Domain input classification, persistence and side effects belong to the caller.
//! This API is provisional until the first real orchestration transition table is validated.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Command,
    Observation,
    /// An observation that ends, rejects or loses the operation being awaited.
    Disruption,
}

#[derive(Debug, Clone, Copy)]
pub struct Row<S: 'static, K: 'static> {
    pub from: S,
    pub on: K,
    pub to: S,
    /// Explains an exceptional self-loop or other special transition. Empty when unnecessary.
    pub why: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Table<S: 'static, K: 'static> {
    pub name: &'static str,
    pub states: &'static [S],
    pub inputs: &'static [(K, Class)],
    pub initial: &'static [S],
    /// States awaiting progress; disruptions must leave them unless the row explains why not.
    pub transient: &'static [S],
    pub terminal: &'static [S],
    pub rows: &'static [Row<S, K>],
}

/// No row matches this state and input. A checked table never rejects a declared observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected<S, K> {
    pub table: &'static str,
    pub from: S,
    pub on: K,
}
impl<S: fmt::Debug, K: fmt::Debug> fmt::Display for Rejected<S, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?} rejects {:?}", self.table, self.from, self.on)
    }
}
impl<S: fmt::Debug, K: fmt::Debug> std::error::Error for Rejected<S, K> {}

impl<S: Copy + Eq + fmt::Debug, K: Copy + Eq + fmt::Debug> Table<S, K> {
    /// Looks up one transition without modifying the table or recording coverage.
    /// Validate with [`check::table`]; this lookup does not repair or validate malformed tables.
    pub fn step(&self, from: S, on: K) -> Result<S, Rejected<S, K>> {
        self.rows
            .iter()
            .find(|r| r.from == from && r.on == on)
            .map(|r| r.to)
            .ok_or(Rejected {
                table: self.name,
                from,
                on,
            })
    }
}

/// Locations are zero-based indices in the corresponding table slices. No domain types or
/// formatted strings are needed to inspect a structural error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    DuplicateState { first: usize, second: usize },
    DuplicateInput { first: usize, second: usize },
    UnknownInitial { initial: usize },
    UnknownTerminal { terminal: usize },
    UnknownTransient { transient: usize },
    UnknownFrom { row: usize },
    UnknownTo { row: usize },
    UnknownInput { row: usize },
    DuplicateTransition { first: usize, second: usize },
    TerminalTransition { row: usize },
    Unreachable { state: usize },
    NoTerminalPath { state: usize },
    MissingObservation { state: usize, input: usize },
    UnexplainedDisruptionSelfLoop { row: usize },
}

pub mod check {
    use super::{Class, Table, Violation};

    fn reachable(edges: &[Vec<usize>], roots: impl Iterator<Item = usize>) -> Vec<bool> {
        let mut seen = vec![false; edges.len()];
        let mut pending: Vec<_> = roots.collect();
        while let Some(state) = pending.pop() {
            if !seen[state] {
                seen[state] = true;
                pending.extend(&edges[state]);
            }
        }
        seen
    }

    fn declarations<S: Eq, K: Eq>(t: &Table<S, K>) -> Vec<Violation> {
        use Violation::*;
        let mut errors = Vec::new();
        for (second, state) in t.states.iter().enumerate() {
            if let Some(first) = t.states[..second].iter().position(|s| s == state) {
                errors.push(DuplicateState { first, second });
            }
        }
        for (second, (input, _)) in t.inputs.iter().enumerate() {
            if let Some(first) = t.inputs[..second].iter().position(|(k, _)| k == input) {
                errors.push(DuplicateInput { first, second });
            }
        }
        for (initial, state) in t.initial.iter().enumerate() {
            if !t.states.contains(state) {
                errors.push(UnknownInitial { initial });
            }
        }
        for (terminal, state) in t.terminal.iter().enumerate() {
            if !t.states.contains(state) {
                errors.push(UnknownTerminal { terminal });
            }
        }
        for (transient, state) in t.transient.iter().enumerate() {
            if !t.states.contains(state) {
                errors.push(UnknownTransient { transient });
            }
        }
        errors
    }

    /// Checks declarations, determinism, terminal behavior, reachability and observation totality.
    /// Both observation classes must be total; transient disruption self-loops need a reason.
    /// Only rows with declared endpoints and inputs participate in the graph checks.
    pub fn table<S: Copy + Eq, K: Copy + Eq>(t: &Table<S, K>) -> Vec<Violation> {
        use Violation::*;
        let mut errors = declarations(t);
        let mut forward = vec![Vec::new(); t.states.len()];
        let mut backward = vec![Vec::new(); t.states.len()];
        for (row, r) in t.rows.iter().enumerate() {
            let from = t.states.iter().position(|s| *s == r.from);
            let to = t.states.iter().position(|s| *s == r.to);
            let class = t.inputs.iter().find(|(k, _)| *k == r.on).map(|(_, c)| *c);
            if from.is_none() {
                errors.push(UnknownFrom { row });
            }
            if to.is_none() {
                errors.push(UnknownTo { row });
            }
            if class.is_none() {
                errors.push(UnknownInput { row });
            }
            if let Some(first) = t.rows[..row]
                .iter()
                .position(|p| p.from == r.from && p.on == r.on)
            {
                errors.push(DuplicateTransition { first, second: row });
            }
            if t.terminal.contains(&r.from)
                && (!matches!(class, Some(Class::Observation | Class::Disruption))
                    || r.to != r.from)
            {
                errors.push(TerminalTransition { row });
            }
            if t.transient.contains(&r.from)
                && class == Some(Class::Disruption)
                && r.to == r.from
                && r.why.trim().is_empty()
            {
                errors.push(UnexplainedDisruptionSelfLoop { row });
            }
            if let (Some(from), Some(to), Some(_)) = (from, to, class) {
                forward[from].push(to);
                backward[to].push(from);
            }
        }
        let roots = |states: &[S]| {
            states
                .iter()
                .filter_map(|state| t.states.iter().position(|s| s == state))
                .collect::<Vec<_>>()
        };
        let reached = reachable(&forward, roots(t.initial).into_iter());
        let ends = reachable(&backward, roots(t.terminal).into_iter());
        for (state, s) in t.states.iter().enumerate() {
            if !reached[state] {
                errors.push(Unreachable { state });
            }
            if !t.terminal.contains(s) && !ends[state] {
                errors.push(NoTerminalPath { state });
            }
            for (input, (k, class)) in t.inputs.iter().enumerate() {
                if matches!(class, Class::Observation | Class::Disruption)
                    && !t.rows.iter().any(|r| r.from == *s && r.on == *k)
                {
                    errors.push(MissingObservation { state, input });
                }
            }
        }
        errors
    }
}
