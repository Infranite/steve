//! Stable local command identities. These types do not change any wire message.
use crate::{AttemptId, OperationId, SessionId, TurnId, WorkspaceRef, derived_id};
use steve_types::DecisionId;

/// Each purpose owns its subject type; invalid subject/purpose pairs cannot be constructed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Intent {
    Prepare(SessionId),
    Open(SessionId),
    Configure(SessionId),
    Release(SessionId),
    StartTurn(TurnId),
    Cancel(AttemptId),
    Abandon(AttemptId),
    Recover(OperationId),
    Answer(DecisionId),
    Tidy(WorkspaceRef),
}

impl Intent {
    pub const fn purpose(&self) -> &'static str {
        match self {
            Self::Prepare(_) => "prepare",
            Self::Open(_) => "open",
            Self::Configure(_) => "configure",
            Self::Release(_) => "release",
            Self::StartTurn(_) => "start_turn",
            Self::Cancel(_) => "cancel",
            Self::Abandon(_) => "abandon",
            Self::Recover(_) => "recover",
            Self::Answer(_) => "answer",
            Self::Tidy(_) => "tidy",
        }
    }

    fn subject(&self) -> String {
        match self {
            Self::Prepare(id) | Self::Open(id) | Self::Configure(id) | Self::Release(id) => {
                id.to_string()
            }
            Self::StartTurn(id) => id.to_string(),
            Self::Cancel(id) | Self::Abandon(id) => id.to_string(),
            Self::Recover(id) => id.to_string(),
            Self::Answer(id) => id.to_string(),
            // Length framing distinguishes ("a:b", "c") from ("a", "b:c").
            Self::Tidy(w) => format!(
                "{}:{}:{}:{}",
                w.node.as_str().len(),
                w.node,
                w.id.as_str().len(),
                w.id
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IntentKey {
    pub intent: Intent,
    pub revision: u32,
}

impl IntentKey {
    /// Permanent format: <purpose>:<subject>:r<revision>, at most 120 bytes.
    /// The reserved <purpose>:: namespace uses derived_id's 128-bit SHA-256 prefix.
    /// Hash input is ":<subject>:r<revision>" (the leading colon forces hashed form).
    pub fn op(&self) -> OperationId {
        let subject = self.intent.subject();
        let suffix = format!("{subject}:r{}", self.revision);
        let plain = format!("{}:{suffix}", self.intent.purpose());
        if plain.len() <= 120 && !subject.starts_with(':') {
            if let Ok(id) = OperationId::new(plain) {
                return id;
            }
        }
        OperationId::new(derived_id(self.intent.purpose(), &format!(":{suffix}")))
            .expect("closed purpose and derived_id produce a bounded ASCII identifier")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeId, WorkspaceId};

    fn examples() -> Vec<(Intent, &'static str)> {
        let s = SessionId::new("s_1").unwrap();
        let a = AttemptId::new("a_1").unwrap();
        vec![
            (Intent::Prepare(s.clone()), "prepare:s_1:r7"),
            (Intent::Open(s.clone()), "open:s_1:r7"),
            (Intent::Configure(s.clone()), "configure:s_1:r7"),
            (Intent::Release(s), "release:s_1:r7"),
            (
                Intent::StartTurn(TurnId::new("t_1").unwrap()),
                "start_turn:t_1:r7",
            ),
            (Intent::Cancel(a.clone()), "cancel:a_1:r7"),
            (Intent::Abandon(a), "abandon:a_1:r7"),
            (
                Intent::Recover(OperationId::new("op_1").unwrap()),
                "recover:op_1:r7",
            ),
            (Intent::Answer("d_1".into()), "answer:d_1:r7"),
            (tidy("n_1", "w_1"), "tidy:3:n_1:3:w_1:r7"),
        ]
    }

    fn tidy(node: &str, id: &str) -> Intent {
        Intent::Tidy(WorkspaceRef {
            node: NodeId::new(node).unwrap(),
            id: WorkspaceId::new(id).unwrap(),
        })
    }

    #[test]
    fn every_purpose_has_a_byte_exact_golden_and_a_unique_name() {
        let cases = examples();
        for (i, (intent, golden)) in cases.iter().enumerate() {
            let key = IntentKey {
                intent: intent.clone(),
                revision: 7,
            };
            assert_eq!(key.op().as_str().as_bytes(), golden.as_bytes());
            for (other, _) in &cases[..i] {
                assert_ne!(intent.purpose(), other.purpose());
            }
            assert_ne!(
                key.op(),
                IntentKey {
                    revision: 8,
                    ..key.clone()
                }
                .op()
            );
        }
    }

    #[test]
    fn workspace_subject_is_unambiguous_and_includes_the_node() {
        let op = |intent| {
            IntentKey {
                intent,
                revision: 0,
            }
            .op()
        };
        assert_ne!(op(tidy("a:b", "c")), op(tidy("a", "b:c")));
        assert_ne!(op(tidy("n1", "w")), op(tidy("n2", "w")));
    }

    #[test]
    fn hashing_starts_at_121_bytes_and_reserves_its_namespace() {
        let key = |s: String| IntentKey {
            intent: Intent::Open(SessionId::new(s).unwrap()),
            revision: 0,
        };
        assert_eq!(
            key("x".repeat(112)).op().as_str(),
            format!("open:{}:r0", "x".repeat(112))
        );
        // open: + 113-byte subject + :r0 = 121 bytes.
        assert_eq!(
            key("x".repeat(113)).op().as_str(),
            "open::038c9209ef410492448d6eca3b077be2"
        );
        let forged = key("x".repeat(113))
            .op()
            .as_str()
            .strip_prefix("open:")
            .unwrap()
            .to_owned();
        assert_ne!(key(forged).op(), key("x".repeat(113)).op());
        assert!(key("x".repeat(128)).op().as_str().starts_with("open::"));
    }

    #[test]
    fn unconstrained_decision_labels_still_make_valid_distinct_operations() {
        let key = |id: &str| IntentKey {
            intent: Intent::Answer(id.into()),
            revision: u32::MAX,
        };
        assert!(
            key("not an opaque id")
                .op()
                .as_str()
                .starts_with("answer::")
        );
        assert_ne!(key("one").op(), key("two").op());
    }
}
