//! Stable local command identities. These types do not change any wire message.
use crate::{AttemptId, Digest, OperationId, SessionId, TurnId, WorkspaceRef};
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
    /// Permanent plain format: <purpose>:<subject>:r<revision>, at most 120 bytes.
    /// Reserved long/escaped format: <purpose>::<24 readable chars>...<128-bit digest>:r<n>.
    /// Hash the full UTF-8 subject; keep the revision visible and reserve the double colon.
    pub fn op(&self) -> OperationId {
        let subject = self.intent.subject();
        let plain = format!("{}:{subject}:r{}", self.intent.purpose(), self.revision);
        if plain.len() <= 120
            && !subject.starts_with(':')
            && let Ok(id) = OperationId::new(plain)
        {
            return id;
        }
        let readable: String = subject
            .chars()
            .take(24)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let digest = Digest::sha256(subject.as_bytes());
        OperationId::new(format!(
            "{}::{readable}...{}:r{}",
            self.intent.purpose(),
            &digest.as_str()[7..39],
            self.revision
        ))
        .expect("closed purpose, ASCII prefix and digest produce a bounded identifier")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeId, WorkspaceId};

    // One declaration generates both the exhaustive match and its test samples.
    // Adding a variant without an arm fails compilation; no parallel list can omit a sample.
    macro_rules! intent_examples {
        ($($variant:ident($subject:expr) => $golden:literal),+ $(,)?) => {
            fn examples(intent: &Intent) -> &'static str {
                match intent {
                    $(Intent::$variant(_) => $golden),+
                }
            }

            fn intents() -> Vec<Intent> {
                vec![$(Intent::$variant($subject)),+]
            }
        };
    }

    intent_examples! {
        Prepare(SessionId::new("s_1").unwrap()) => "prepare:s_1:r7",
        Open(SessionId::new("s_1").unwrap()) => "open:s_1:r7",
        Configure(SessionId::new("s_1").unwrap()) => "configure:s_1:r7",
        Release(SessionId::new("s_1").unwrap()) => "release:s_1:r7",
        StartTurn(TurnId::new("t_1").unwrap()) => "start_turn:t_1:r7",
        Cancel(AttemptId::new("a_1").unwrap()) => "cancel:a_1:r7",
        Abandon(AttemptId::new("a_1").unwrap()) => "abandon:a_1:r7",
        Recover(OperationId::new("op_1").unwrap()) => "recover:op_1:r7",
        Answer("d_1".into()) => "answer:d_1:r7",
        Tidy(WorkspaceRef {
            node: NodeId::new("n_1").unwrap(),
            id: WorkspaceId::new("w_1").unwrap(),
        }) => "tidy:3:n_1:3:w_1:r7",
    }

    fn tidy(node: &str, id: &str) -> Intent {
        Intent::Tidy(WorkspaceRef {
            node: NodeId::new(node).unwrap(),
            id: WorkspaceId::new(id).unwrap(),
        })
    }

    #[test]
    fn every_purpose_has_a_byte_exact_golden_and_a_unique_name() {
        let cases = intents();
        for (i, intent) in cases.iter().enumerate() {
            let golden = examples(intent);
            let key = IntentKey {
                intent: intent.clone(),
                revision: 7,
            };
            assert_eq!(key.op().as_str().as_bytes(), golden.as_bytes());
            for other in &cases[..i] {
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
            "open::xxxxxxxxxxxxxxxxxxxxxxxx...b8983b329f09f5296bc9951e1ec7ad91:r0"
        );
        let forged = key("x".repeat(113))
            .op()
            .as_str()
            .strip_prefix("open:")
            .unwrap()
            .to_owned();
        assert_ne!(key(forged).op(), key("x".repeat(113)).op());
        let long = key("x".repeat(128)).op();
        assert!(
            long.as_str()
                .starts_with("open::xxxxxxxxxxxxxxxxxxxxxxxx...")
        );
        assert!(long.as_str().ends_with(":r0"));
        assert!(long.as_str().len() <= 120);
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
