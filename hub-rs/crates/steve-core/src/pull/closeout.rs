use super::Pr;

pub use steve_types::{Closeout, PullChecks as Checks, PullSnapshot as Snapshot};

impl Pr {
    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot {
            number: self.number,
            url: self.url.clone(),
            head: self.head.clone(),
            state: self.state.clone(),
            checks: self.checks.clone(),
        }
    }
}
