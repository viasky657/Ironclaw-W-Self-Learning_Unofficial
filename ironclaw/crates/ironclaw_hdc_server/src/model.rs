use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use zeroize::Zeroize;

use crate::types::WriteOutcome;

/// HDC DSV model — bag-of-characters hypervector encoding.
///
/// Stored as `bincode` (not Python pickle) so loading the model file
/// cannot execute arbitrary code.
///
/// The model state is wrapped in `Arc<RwLock<>>` for concurrent read access
/// from multiple request handlers.
#[derive(serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
pub struct HdcDsvModel {
    /// Hypervector dimension.
    dimension: usize,
    /// Good-write prototype vector.
    good_prototype: Vec<f32>,
    /// Bad-write prototype vector.
    bad_prototype: Vec<f32>,
    /// Number of training samples seen.
    train_count: u64,
    /// Model version string.
    version: String,
}

impl HdcDsvModel {
    /// Create a new untrained model with the given dimension.
    pub fn new(dimension: usize) -> Self {
        Self {
            dimension,
            good_prototype: vec![0.0; dimension],
            bad_prototype: vec![0.0; dimension],
            train_count: 0,
            version: "1.0.0".to_string(),
        }
    }

    /// Encode content as a bag-of-characters hypervector.
    ///
    /// Each character is mapped to a position in the hypervector using
    /// a deterministic hash. The resulting vector is L2-normalized.
    fn encode(&self, content: &str) -> Vec<f32> {
        let mut hv = vec![0.0f32; self.dimension];
        for ch in content.chars() {
            let idx = (ch as usize) % self.dimension;
            hv[idx] += 1.0;
        }
        // L2 normalize.
        let norm: f32 = hv.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut hv {
                *x /= norm;
            }
        }
        hv
    }

    /// Compute cosine similarity between two vectors.
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm_a == 0.0 || norm_b == 0.0 {
            return 0.0;
        }
        dot / (norm_a * norm_b)
    }

    /// Score content — returns a value in [-1.0, 1.0].
    ///
    /// Positive values indicate good-write similarity.
    /// Negative values indicate bad-write similarity.
    pub fn score(&self, content: &str) -> f32 {
        let hv = self.encode(content);
        let good_sim = Self::cosine_similarity(&hv, &self.good_prototype);
        let bad_sim = Self::cosine_similarity(&hv, &self.bad_prototype);
        good_sim - bad_sim
    }

    /// Online learning — update the prototype vectors.
    pub fn train(&mut self, content: &str, outcome: WriteOutcome) {
        let hv = self.encode(content);
        let alpha = 0.01f32; // Learning rate.
        match outcome {
            WriteOutcome::GoodWrite => {
                for (p, h) in self.good_prototype.iter_mut().zip(hv.iter()) {
                    *p += alpha * h;
                }
            }
            WriteOutcome::BadWrite => {
                for (p, h) in self.bad_prototype.iter_mut().zip(hv.iter()) {
                    *p += alpha * h;
                }
            }
        }
        self.train_count += 1;
    }

    /// Save the model to a file using `bincode` serialization.
    ///
    /// Sets file permissions to 0600 (owner read/write only) on Unix.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let bytes = bincode::encode_to_vec(self, bincode::config::standard())?;
        std::fs::write(path, &bytes)?;

        // Set 0600 permissions on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, perms)?;
        }

        tracing::info!(
            path = %path.display(),
            train_count = self.train_count,
            "HDC model saved"
        );
        Ok(())
    }

    /// Load the model from a file using `bincode` deserialization.
    ///
    /// **No pickle, no arbitrary code execution on load.**
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let (model, _) = bincode::decode_from_slice::<Self, _>(&bytes, bincode::config::standard())?;
        tracing::info!(
            path = %path.display(),
            train_count = model.train_count,
            "HDC model loaded"
        );
        Ok(model)
    }

    pub fn train_count(&self) -> u64 {
        self.train_count
    }

    pub fn version(&self) -> &str {
        &self.version
    }
}

/// Thread-safe wrapper around `HdcDsvModel`.
pub type SharedModel = Arc<RwLock<HdcDsvModel>>;

pub fn new_shared_model(dimension: usize) -> SharedModel {
    Arc::new(RwLock::new(HdcDsvModel::new(dimension)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn score_returns_value_in_range() {
        let model = HdcDsvModel::new(1024);
        let score = model.score("hello world");
        assert!(score >= -1.0 && score <= 1.0, "score {} out of range", score);
    }

    #[test]
    fn train_changes_score() {
        let mut model = HdcDsvModel::new(1024);
        let content = "this is a good skill write";
        let before = model.score(content);
        model.train(content, WriteOutcome::GoodWrite);
        let after = model.score(content);
        // After training as good, the good score should increase.
        assert!(after >= before, "score should increase after good training");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("model.bin");

        let mut model = HdcDsvModel::new(512);
        model.train("test content", WriteOutcome::GoodWrite);
        model.save(&path).unwrap();

        let loaded = HdcDsvModel::load(&path).unwrap();
        assert_eq!(loaded.train_count(), 1);
        assert_eq!(loaded.version(), "1.0.0");
        // Scores should be identical after roundtrip.
        assert_eq!(model.score("test"), loaded.score("test"));
    }

    #[test]
    fn load_rejects_invalid_data() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.bin");
        std::fs::write(&path, b"not valid bincode data").unwrap();
        assert!(HdcDsvModel::load(&path).is_err());
    }
}
