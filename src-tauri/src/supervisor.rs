use std::time::Duration;

pub const MAX_CONCURRENT_AGENTS: usize = 5;
const MAX_RESTARTS: u32 = 5;
const CAP_SECS: u64 = 30;

#[derive(Default)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub fn new() -> Self {
        Self { attempt: 0 }
    }

    pub fn capped(secs: u64) -> Duration {
        Duration::from_secs(secs.min(CAP_SECS))
    }

    pub fn next_delay(&mut self) -> Option<Duration> {
        if self.attempt >= MAX_RESTARTS {
            return None;
        }
        let secs = 1u64 << self.attempt; // 1,2,4,8,16
        self.attempt += 1;
        Some(Self::capped(secs))
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

pub struct ConcurrencyGate {
    active: usize,
    max: usize,
}

impl ConcurrencyGate {
    pub fn new(max: usize) -> Self {
        Self { active: 0, max }
    }

    pub fn try_acquire(&mut self) -> bool {
        if self.active < self.max {
            self.active += 1;
            true
        } else {
            false
        }
    }

    pub fn release(&mut self) {
        if self.active > 0 {
            self.active -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn backoff_grows_then_faults_after_five() {
        let mut b = Backoff::new();
        assert_eq!(b.next_delay(), Some(Duration::from_secs(1)));
        assert_eq!(b.next_delay(), Some(Duration::from_secs(2)));
        assert_eq!(b.next_delay(), Some(Duration::from_secs(4)));
        assert_eq!(b.next_delay(), Some(Duration::from_secs(8)));
        assert_eq!(b.next_delay(), Some(Duration::from_secs(16)));
        assert_eq!(b.next_delay(), None); // 第 6 次 → 故障态
    }

    #[test]
    fn backoff_caps_at_thirty_seconds() {
        // 若上限逻辑生效：即便指数超过 30s 也封顶（此处 5 次内不会触顶，验证 cap 函数）
        assert_eq!(Backoff::capped(64), Duration::from_secs(30));
        assert_eq!(Backoff::capped(8), Duration::from_secs(8));
    }

    #[test]
    fn reset_clears_attempts() {
        let mut b = Backoff::new();
        b.next_delay();
        b.next_delay();
        b.reset();
        assert_eq!(b.next_delay(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn concurrency_gate_blocks_over_max() {
        let mut g = ConcurrencyGate::new(2);
        assert!(g.try_acquire());
        assert!(g.try_acquire());
        assert!(!g.try_acquire()); // 满了
        g.release();
        assert!(g.try_acquire());
    }
}
