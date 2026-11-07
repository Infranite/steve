//! Conservative cleanup preview, with no paths, filesystem effects or deletion authority.
//!
//! Evidence must come from the current Node/Hub lifecycle, not directory names, asynchronous
//! disk stock, an idle harness or a successful turn. A candidate is NOT a reusable deletion
//! token: an executor must reacquire exclusion, recheck every fact, anchor fds and use same-volume
//! trash. Until those producers/executor exist this module has no runtime callers.

use serde::{Deserialize, Serialize};

/// Unknown is deliberately distinct from a negative finding; neither satisfies a prerequisite.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Confirmed,
    Refuted,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    GitWork,
    ScratchWork,
    RebuildableOutput,
    SupersededSeed,
    FailedPreparation,
    FinishedTemporary,
    RegisteredTrash,
}

impl Kind {
    fn business(self) -> bool {
        matches!(
            self,
            Self::GitWork | Self::ScratchWork | Self::RebuildableOutput | Self::SupersededSeed
        )
    }
}

/// Facts for ONE registered object at ONE generation. These fields are not supplied by a
/// harness and do not grant permissions. Producers must bind receipts/inventory to that object,
/// principal, revision and generation before asserting Confirmed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub managed: Check,
    /// Physical object owned for reclamation, not an adopted/external project directory.
    /// Merely registering an Existing workspace must never confirm this.
    pub owned_for_reclamation: Check,
    pub current_generation: Check,
    /// Current authorization to reclaim this object, distinct from permission to start a run.
    /// Neither a revoked run grant nor the cleanup request alone proves this.
    pub cleanup_authorized: Check,
    pub processes_stopped: Check,
    /// No running or unknown execution, including after reconnect/restart.
    pub execution_resolved: Check,
    /// A current, authoritative Hub in-use set; an empty but stale set is not confirmed.
    pub inventory_fresh: Check,
    pub unreferenced: Check,
    pub lifecycle_closed: Check,
    /// Git clean OR a successful park of all dirty/untracked content.
    pub dirty_content_saved: Check,
    /// Every required destination has the branch and park refs; one failure refutes this.
    pub synchronized: Check,
    pub artifacts_saved: Check,
    /// Scratch's sole substantive copy is snapshotted successfully, regardless of dirty status.
    pub scratch_saved: Check,
    /// The exact directory is disposable in a validated toolchain definition, not just its name.
    pub disposable: Check,
    /// An allowed capacity-recovery trigger, not merely periodic housekeeping or elapsed time.
    pub output_reclaim_trigger_allowed: Check,
    /// From current lifecycle activity, not a directory mtime or disk observation timestamp.
    pub idle_ms: Option<u64>,
    /// Same principal/project/toolchain/directory lineage, superseded by a registered key.
    pub seed_superseded: Check,
    /// The configured keep set has been resolved and excludes this key (including the default
    /// branch's current key and the recently used key). Supersession alone is insufficient.
    pub seed_retention_allows: Check,
    /// Ended, never unknown; FailedPreparation additionally requires confirmed failure.
    pub operation_ended: Check,
    pub operation_failed: Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// A caller's configured threshold; no hidden clamp or permanent disk quota.
    pub output_idle_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    Managed,
    OwnedForReclamation,
    CurrentGeneration,
    CleanupAuthorized,
    ProcessesStopped,
    ExecutionResolved,
    InventoryFresh,
    Unreferenced,
    LifecycleClosed,
    DirtyContentSaved,
    Synchronized,
    ArtifactsSaved,
    ScratchSaved,
    Disposable,
    OutputReclaimTriggerAllowed,
    IdleThreshold,
    SeedSuperseded,
    SeedRetentionAllows,
    OperationEnded,
    OperationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub requirement: Requirement,
    pub found: Check,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "preview", content = "gaps", rename_all = "snake_case")]
pub enum Preview {
    Keep(Vec<Gap>),
    /// Only a dry-run candidate; no path, receipt, lease or persisted deletion token is issued.
    RecheckBeforeTrash,
}

/// Evaluate all missing requirements for explanation; never turn an unknown into "not in use".
/// `kind` must also come from that object's managed registration, never its path/name.
pub fn preview(kind: Kind, e: &Evidence, policy: Policy) -> Preview {
    use Requirement as R;
    let mut gaps = Vec::new();
    let mut require = |requirement, found| {
        if found != Check::Confirmed {
            gaps.push(Gap { requirement, found });
        }
    };
    require(R::Managed, e.managed);
    require(R::OwnedForReclamation, e.owned_for_reclamation);
    require(R::CurrentGeneration, e.current_generation);
    require(R::CleanupAuthorized, e.cleanup_authorized);
    require(R::ProcessesStopped, e.processes_stopped);
    require(R::ExecutionResolved, e.execution_resolved);
    if kind.business() {
        require(R::InventoryFresh, e.inventory_fresh);
        require(R::Unreferenced, e.unreferenced);
    }
    match kind {
        Kind::GitWork => {
            require(R::LifecycleClosed, e.lifecycle_closed);
            require(R::DirtyContentSaved, e.dirty_content_saved);
            require(R::Synchronized, e.synchronized);
            require(R::ArtifactsSaved, e.artifacts_saved);
        }
        Kind::ScratchWork => {
            require(R::LifecycleClosed, e.lifecycle_closed);
            require(R::ScratchSaved, e.scratch_saved);
        }
        Kind::RebuildableOutput => {
            require(R::Disposable, e.disposable);
            require(
                R::OutputReclaimTriggerAllowed,
                e.output_reclaim_trigger_allowed,
            );
            require(
                R::IdleThreshold,
                e.idle_ms.map_or(Check::Unknown, |idle| {
                    if idle >= policy.output_idle_ms {
                        Check::Confirmed
                    } else {
                        Check::Refuted
                    }
                }),
            );
        }
        Kind::SupersededSeed => {
            require(R::SeedSuperseded, e.seed_superseded);
            require(R::SeedRetentionAllows, e.seed_retention_allows);
        }
        Kind::FailedPreparation => {
            require(R::OperationEnded, e.operation_ended);
            require(R::OperationFailed, e.operation_failed);
        }
        Kind::FinishedTemporary | Kind::RegisteredTrash => {
            require(R::OperationEnded, e.operation_ended);
        }
    }
    if gaps.is_empty() {
        Preview::RecheckBeforeTrash
    } else {
        Preview::Keep(gaps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: Policy = Policy {
        output_idle_ms: 1000,
    };

    fn confirmed() -> Evidence {
        Evidence {
            managed: Check::Confirmed,
            owned_for_reclamation: Check::Confirmed,
            current_generation: Check::Confirmed,
            cleanup_authorized: Check::Confirmed,
            processes_stopped: Check::Confirmed,
            execution_resolved: Check::Confirmed,
            inventory_fresh: Check::Confirmed,
            unreferenced: Check::Confirmed,
            lifecycle_closed: Check::Confirmed,
            dirty_content_saved: Check::Confirmed,
            synchronized: Check::Confirmed,
            artifacts_saved: Check::Confirmed,
            scratch_saved: Check::Confirmed,
            disposable: Check::Confirmed,
            output_reclaim_trigger_allowed: Check::Confirmed,
            idle_ms: Some(1000),
            seed_superseded: Check::Confirmed,
            seed_retention_allows: Check::Confirmed,
            operation_ended: Check::Confirmed,
            operation_failed: Check::Confirmed,
        }
    }

    fn gap(kind: Kind, e: &Evidence, requirement: Requirement, found: Check) {
        let Preview::Keep(gaps) = preview(kind, e, POLICY) else {
            panic!("must retain")
        };
        assert!(gaps.contains(&Gap { requirement, found }), "{gaps:?}");
    }

    #[test]
    fn missing_or_stale_inventory_never_turns_into_an_empty_in_use_set() {
        for kind in [
            Kind::GitWork,
            Kind::ScratchWork,
            Kind::RebuildableOutput,
            Kind::SupersededSeed,
        ] {
            let e = Evidence {
                inventory_fresh: Check::Unknown,
                ..confirmed()
            };
            gap(kind, &e, Requirement::InventoryFresh, Check::Unknown);
            let e = Evidence {
                unreferenced: Check::Refuted,
                ..confirmed()
            };
            gap(kind, &e, Requirement::Unreferenced, Check::Refuted);
        }
    }

    #[test]
    fn unknown_processes_keep_all_objects_including_partials_and_trash() {
        for kind in [
            Kind::GitWork,
            Kind::ScratchWork,
            Kind::RebuildableOutput,
            Kind::SupersededSeed,
            Kind::FailedPreparation,
            Kind::FinishedTemporary,
            Kind::RegisteredTrash,
        ] {
            let e = Evidence {
                processes_stopped: Check::Unknown,
                ..confirmed()
            };
            gap(kind, &e, Requirement::ProcessesStopped, Check::Unknown);
            let e = Evidence {
                execution_resolved: Check::Unknown,
                ..confirmed()
            };
            gap(kind, &e, Requirement::ExecutionResolved, Check::Unknown);
        }
    }

    #[test]
    fn stale_generation_or_missing_cleanup_authority_keeps_even_saved_objects() {
        for kind in [
            Kind::GitWork,
            Kind::ScratchWork,
            Kind::RebuildableOutput,
            Kind::SupersededSeed,
            Kind::FailedPreparation,
            Kind::FinishedTemporary,
            Kind::RegisteredTrash,
        ] {
            let e = Evidence {
                current_generation: Check::Refuted,
                ..confirmed()
            };
            gap(kind, &e, Requirement::CurrentGeneration, Check::Refuted);
            let e = Evidence {
                cleanup_authorized: Check::Unknown,
                ..confirmed()
            };
            gap(kind, &e, Requirement::CleanupAuthorized, Check::Unknown);
        }
    }

    #[test]
    fn a_registered_but_adopted_external_directory_is_never_a_cleanup_candidate() {
        let e = Evidence {
            owned_for_reclamation: Check::Refuted,
            ..confirmed()
        };
        for kind in [
            Kind::GitWork,
            Kind::ScratchWork,
            Kind::RebuildableOutput,
            Kind::SupersededSeed,
            Kind::FailedPreparation,
            Kind::FinishedTemporary,
            Kind::RegisteredTrash,
        ] {
            gap(kind, &e, Requirement::OwnedForReclamation, Check::Refuted);
        }
    }

    #[test]
    fn clean_or_finished_scratch_still_requires_its_sole_copy_snapshot() {
        let e = Evidence {
            scratch_saved: Check::Refuted,
            ..confirmed()
        };
        gap(
            Kind::ScratchWork,
            &e,
            Requirement::ScratchSaved,
            Check::Refuted,
        );
        let e = Evidence {
            scratch_saved: Check::Unknown,
            ..confirmed()
        };
        gap(
            Kind::ScratchWork,
            &e,
            Requirement::ScratchSaved,
            Check::Unknown,
        );
    }

    #[test]
    fn git_work_keeps_all_unsaved_unparked_unsynced_or_unsnapshotted_content() {
        for requirement in [
            Requirement::DirtyContentSaved,
            Requirement::Synchronized,
            Requirement::ArtifactsSaved,
        ] {
            let mut e = confirmed();
            match requirement {
                Requirement::DirtyContentSaved => e.dirty_content_saved = Check::Refuted,
                Requirement::Synchronized => e.synchronized = Check::Refuted,
                Requirement::ArtifactsSaved => e.artifacts_saved = Check::Refuted,
                _ => unreachable!(),
            }
            gap(Kind::GitWork, &e, requirement, Check::Refuted);
        }
    }

    #[test]
    fn idle_is_configurable_and_neither_a_name_nor_a_mtime_proves_disposability() {
        let e = Evidence {
            disposable: Check::Unknown,
            ..confirmed()
        };
        gap(
            Kind::RebuildableOutput,
            &e,
            Requirement::Disposable,
            Check::Unknown,
        );
        let e = Evidence {
            idle_ms: Some(999),
            ..confirmed()
        };
        gap(
            Kind::RebuildableOutput,
            &e,
            Requirement::IdleThreshold,
            Check::Refuted,
        );
        assert_eq!(
            preview(
                Kind::RebuildableOutput,
                &e,
                Policy {
                    output_idle_ms: 999
                }
            ),
            Preview::RecheckBeforeTrash
        );
    }

    #[test]
    fn local_cleanup_requires_registered_resolved_objects_but_not_a_hub_connection() {
        for kind in [
            Kind::FailedPreparation,
            Kind::FinishedTemporary,
            Kind::RegisteredTrash,
        ] {
            let e = Evidence {
                inventory_fresh: Check::Unknown,
                unreferenced: Check::Unknown,
                ..confirmed()
            };
            assert_eq!(preview(kind, &e, POLICY), Preview::RecheckBeforeTrash);
            let e = Evidence {
                managed: Check::Unknown,
                ..e
            };
            gap(kind, &e, Requirement::Managed, Check::Unknown);
        }
    }

    #[test]
    fn supersession_does_not_override_the_seed_keep_set() {
        for found in [Check::Refuted, Check::Unknown] {
            let e = Evidence {
                seed_retention_allows: found,
                ..confirmed()
            };
            gap(
                Kind::SupersededSeed,
                &e,
                Requirement::SeedRetentionAllows,
                found,
            );
        }
    }

    #[test]
    fn ordinary_housekeeping_does_not_reclaim_even_idle_disposable_output() {
        for found in [Check::Refuted, Check::Unknown] {
            let e = Evidence {
                output_reclaim_trigger_allowed: found,
                ..confirmed()
            };
            gap(
                Kind::RebuildableOutput,
                &e,
                Requirement::OutputReclaimTriggerAllowed,
                found,
            );
        }
    }
}
