use std::path::Path;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// Before-state snapshot for a single skill file.
struct SkillSnapshot {
    skill_name: String,
    file_path: String,
    /// Content before the write, or None if the file did not exist.
    /// Wrapped in `Zeroizing` so the content is zeroed on drop.
    content_before: Option<Zeroizing<String>>,
    event_id: String,
}

/// Internal mutable state for the rollback manager.
struct RollbackState {
    snapshots: Vec<SkillSnapshot>,
    committed: bool,
    rolled_back: bool,
}

/// Manages rollback of self-improvement writes for a single job.
///
/// Uses `Arc<Mutex<RollbackState>>` for thread-safe access.
/// `content_before` is wrapped in `zeroize::Zeroizing` so skill content
/// is zeroed from memory when the snapshot is dropped.
pub struct RollbackManager {
    pub job_id: String,
    skills_path: String,
    state: Arc<Mutex<RollbackState>>,
}

impl RollbackManager {
    pub fn new(job_id: String, skills_path: Option<String>) -> Self {
        let skills_path = skills_path
            .or_else(|| std::env::var("SKILLS_VOLUME_PATH").ok())
            .unwrap_or_else(|| "/hermes-skills".to_string());

        Self {
            job_id,
            skills_path,
            state: Arc::new(Mutex::new(RollbackState {
                snapshots: Vec::new(),
                committed: false,
                rolled_back: false,
            })),
        }
    }

    /// Record the before-state of a skill file.
    ///
    /// Call this before applying any write. The snapshot is used to restore
    /// the file if the job fails.
    pub fn snapshot_skill(
        &self,
        skill_name: &str,
        content_before: Option<String>,
        event_id: &str,
    ) {
        let file_path = format!("{}/{}.md", self.skills_path, skill_name);
        let snapshot = SkillSnapshot {
            skill_name: skill_name.to_string(),
            file_path,
            content_before: content_before.map(Zeroizing::new),
            event_id: event_id.to_string(),
        };
        let mut state = self.state.lock().unwrap();
        state.snapshots.push(snapshot);
        tracing::debug!(
            skill = %skill_name,
            job = %self.job_id,
            event = %event_id,
            "Rollback: snapshot recorded"
        );
    }

    /// Mark all writes as committed.
    ///
    /// Returns `true` if the commit succeeded.
    pub fn commit(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.rolled_back {
            tracing::warn!(job = %self.job_id, "Rollback: cannot commit — already rolled back");
            return false;
        }
        if state.committed {
            return true;
        }
        state.committed = true;
        tracing::info!(
            job = %self.job_id,
            writes = state.snapshots.len(),
            "Rollback: job committed"
        );
        true
    }

    /// Roll back all writes for this job.
    ///
    /// Restores each skill file to its before-state in reverse order
    /// (most recent write first). Returns `true` if the rollback succeeded.
    pub fn rollback(&self, reason: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.committed {
            tracing::warn!(job = %self.job_id, "Rollback: cannot roll back — already committed");
            return false;
        }
        if state.rolled_back {
            return true;
        }

        tracing::info!(
            job = %self.job_id,
            writes = state.snapshots.len(),
            reason = %reason,
            "Rollback: rolling back job"
        );

        let mut errors: Vec<String> = Vec::new();
        // Restore in reverse order (most recent write first).
        for snapshot in state.snapshots.iter().rev() {
            if let Err(e) = restore_skill(snapshot) {
                errors.push(format!("skill '{}': {}", snapshot.skill_name, e));
                tracing::warn!(
                    skill = %snapshot.skill_name,
                    error = %e,
                    "Rollback: failed to restore skill"
                );
            }
        }

        state.rolled_back = true;

        if errors.is_empty() {
            tracing::info!(job = %self.job_id, "Rollback: job rolled back successfully");
            true
        } else {
            tracing::error!(
                job = %self.job_id,
                errors = ?errors,
                "Rollback: job rolled back with errors"
            );
            false
        }
    }

    pub fn snapshot_count(&self) -> usize {
        self.state.lock().unwrap().snapshots.len()
    }

    pub fn is_committed(&self) -> bool {
        self.state.lock().unwrap().committed
    }

    pub fn is_rolled_back(&self) -> bool {
        self.state.lock().unwrap().rolled_back
    }
}

fn restore_skill(snapshot: &SkillSnapshot) -> std::io::Result<()> {
    let path = Path::new(&snapshot.file_path);
    match &snapshot.content_before {
        None => {
            // File did not exist before — delete it.
            if path.exists() {
                std::fs::remove_file(path)?;
                tracing::debug!(skill = %snapshot.skill_name, "Rollback: deleted new skill");
            }
        }
        Some(content) => {
            // Restore the previous content.
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, content.as_bytes())?;
            tracing::debug!(
                skill = %snapshot.skill_name,
                bytes = content.len(),
                "Rollback: restored skill"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn commit_marks_committed() {
        let rm = RollbackManager::new("job-1".to_string(), Some("/tmp".to_string()));
        assert!(!rm.is_committed());
        assert!(rm.commit());
        assert!(rm.is_committed());
    }

    #[test]
    fn rollback_after_commit_fails() {
        let rm = RollbackManager::new("job-2".to_string(), Some("/tmp".to_string()));
        rm.commit();
        assert!(!rm.rollback("test"));
    }

    #[test]
    fn commit_after_rollback_fails() {
        let rm = RollbackManager::new("job-3".to_string(), Some("/tmp".to_string()));
        rm.rollback("test");
        assert!(!rm.commit());
    }

    #[test]
    fn rollback_restores_file() {
        let dir = TempDir::new().unwrap();
        let skills_path = dir.path().to_str().unwrap().to_string();
        let skill_file = dir.path().join("my_skill.md");

        // Write initial content.
        fs::write(&skill_file, "original content").unwrap();

        let rm = RollbackManager::new("job-4".to_string(), Some(skills_path));
        rm.snapshot_skill("my_skill", Some("original content".to_string()), "evt-1");

        // Simulate a write.
        fs::write(&skill_file, "new content").unwrap();

        // Rollback should restore original.
        assert!(rm.rollback("test failure"));
        let restored = fs::read_to_string(&skill_file).unwrap();
        assert_eq!(restored, "original content");
    }

    #[test]
    fn rollback_deletes_new_file() {
        let dir = TempDir::new().unwrap();
        let skills_path = dir.path().to_str().unwrap().to_string();
        let skill_file = dir.path().join("new_skill.md");

        let rm = RollbackManager::new("job-5".to_string(), Some(skills_path));
        rm.snapshot_skill("new_skill", None, "evt-2");

        // Simulate creating the file.
        fs::write(&skill_file, "new skill content").unwrap();

        // Rollback should delete it.
        assert!(rm.rollback("test failure"));
        assert!(!skill_file.exists());
    }

    #[test]
    fn double_rollback_is_idempotent() {
        let rm = RollbackManager::new("job-6".to_string(), Some("/tmp".to_string()));
        assert!(rm.rollback("first"));
        assert!(rm.rollback("second")); // second call returns true (already rolled back)
    }
}
