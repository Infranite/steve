//! Managed skills exchange. Node caches are per workspace and Bot; the immutable snapshot held
//! by one session is deliberately independent of the cache's moving current pointer.

use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiResult, Digest, ErrorCode, Skill, digest_of};

pub const FEATURE_MANAGED_SKILLS: &str = "managed-skills-v1";
pub const SKILLS_ADAPTER_PREFIX: &str = "managed-skills:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedSkill {
    pub id: String,
    pub version: i64,
    pub publication: i64,
    #[serde(flatten)]
    pub body: Skill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSnapshot {
    pub workspace: String,
    pub bot: String,
    pub digest: Digest,
    pub entries: Vec<VersionedSkill>,
}

impl SkillSnapshot {
    pub fn new(workspace: String, bot: String, entries: Vec<VersionedSkill>) -> ApiResult<Self> {
        let digest = digest_of(&(&workspace, &bot, &entries))?;
        let snapshot = Self {
            workspace,
            bot,
            digest,
            entries,
        };
        snapshot.check()?;
        Ok(snapshot)
    }

    pub fn check(&self) -> ApiResult<()> {
        let good_id = |s: &str| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        };
        if !good_id(&self.workspace)
            || !good_id(&self.bot)
            || self
                .entries
                .iter()
                .any(|e| !good_id(&e.id) || e.version < 1 || e.publication < 1)
            || self.entries.windows(2).any(|w| w[0].id >= w[1].id)
        {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "invalid managed skill snapshot",
            ));
        }
        Skill::check_all(
            &self
                .entries
                .iter()
                .map(|e| e.body.clone())
                .collect::<Vec<_>>(),
        )?;
        if digest_of(&(&self.workspace, &self.bot, &self.entries))? != self.digest {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "skill snapshot digest mismatch",
            ));
        }
        Ok(())
    }

    pub fn refs(&self) -> Vec<SkillVersionRef> {
        self.entries
            .iter()
            .map(|e| SkillVersionRef {
                id: e.id.clone(),
                version: e.version,
                name: e.body.name.clone(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillVersionRef {
    pub id: String,
    pub version: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSkills {
    pub generation: i64,
    pub adapter: String,
    pub snapshot: SkillSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCacheReceipt {
    pub generation: i64,
    pub digest: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUseReceipt {
    pub digest: Digest,
    pub materialized: Vec<SkillVersionRef>,
    /// Existing personal/unmanaged content was left intact.
    pub conflicts: Vec<SkillVersionRef>,
}
