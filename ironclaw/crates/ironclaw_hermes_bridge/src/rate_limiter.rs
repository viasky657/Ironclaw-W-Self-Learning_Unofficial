//! Per-job rate limiting for self-improvement writes.
//!
//! Enforces hard caps on the number of skill and memory writes per job.
//! These limits are set at job creation time and cannot be exceeded.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::types::BridgeError;

/// Per-job rate limiter for self-improvement writes.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<RateLimiterInner>,
}

#[derive(Debug)]
struct RateLimiterInner {
    skill_writes: AtomicU32,
    memory_writes: AtomicU32,
    max_skill_writes: u32,
    max_memory_writes: u32,
}

impl RateLimiter {
    /// Create a new rate limiter with the given limits.
    pub fn new(max_skill_writes: u32, max_memory_writes: u32) -> Self {
        Self {
            inner: Arc::new(RateLimiterInner {
                skill_writes: AtomicU32::new(0),
                memory_writes: AtomicU32::new(0),
                max_skill_writes,
                max_memory_writes,
            }),
        }
    }

    /// Attempt to consume a skill write slot.
    ///
    /// Returns `Ok(())` if the write is allowed, or `Err(BridgeError::RateLimitExceeded)`
    /// if the per-job limit has been reached.
    pub fn consume_skill_write(&self) -> Result<(), BridgeError> {
        let current = self.inner.skill_writes.fetch_add(1, Ordering::SeqCst);
        if current >= self.inner.max_skill_writes {
            // Roll back the increment.
            self.inner.skill_writes.fetch_sub(1, Ordering::SeqCst);
            return Err(BridgeError::RateLimitExceeded(format!(
                "Skill write limit reached: {} writes (max {})",
                current, self.inner.max_skill_writes
            )));
        }
        Ok(())
    }

    /// Attempt to consume a memory write slot.
    pub fn consume_memory_write(&self) -> Result<(), BridgeError> {
        let current = self.inner.memory_writes.fetch_add(1, Ordering::SeqCst);
        if current >= self.inner.max_memory_writes {
            self.inner.memory_writes.fetch_sub(1, Ordering::SeqCst);
            return Err(BridgeError::RateLimitExceeded(format!(
                "Memory write limit reached: {} writes (max {})",
                current, self.inner.max_memory_writes
            )));
        }
        Ok(())
    }

    /// Current skill write count.
    pub fn skill_write_count(&self) -> u32 {
        self.inner.skill_writes.load(Ordering::SeqCst)
    }

    /// Current memory write count.
    pub fn memory_write_count(&self) -> u32 {
        self.inner.memory_writes.load(Ordering::SeqCst)
    }

    /// Remaining skill write slots.
    pub fn skill_writes_remaining(&self) -> u32 {
        self.inner
            .max_skill_writes
            .saturating_sub(self.inner.skill_writes.load(Ordering::SeqCst))
    }

    /// Remaining memory write slots.
    pub fn memory_writes_remaining(&self) -> u32 {
        self.inner
            .max_memory_writes
            .saturating_sub(self.inner.memory_writes.load(Ordering::SeqCst))
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(10, 5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_write_limit() {
        let limiter = RateLimiter::new(3, 5);
        assert!(limiter.consume_skill_write().is_ok());
        assert!(limiter.consume_skill_write().is_ok());
        assert!(limiter.consume_skill_write().is_ok());
        // 4th write should fail.
        assert!(limiter.consume_skill_write().is_err());
        assert_eq!(limiter.skill_write_count(), 3);
    }

    #[test]
    fn test_memory_write_limit() {
        let limiter = RateLimiter::new(10, 2);
        assert!(limiter.consume_memory_write().is_ok());
        assert!(limiter.consume_memory_write().is_ok());
        assert!(limiter.consume_memory_write().is_err());
        assert_eq!(limiter.memory_write_count(), 2);
    }

    #[test]
    fn test_remaining_counts() {
        let limiter = RateLimiter::new(5, 3);
        assert_eq!(limiter.skill_writes_remaining(), 5);
        limiter.consume_skill_write().unwrap();
        assert_eq!(limiter.skill_writes_remaining(), 4);
    }
}
