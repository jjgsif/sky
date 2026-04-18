use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::config::WorkerConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureOutcome {
    Backoff(Duration),
    Permanent,
}

pub struct RestartPolicy {
    history: VecDeque<Instant>,
    initial_backoff: Duration,
    pub max_backoff: Duration,
    failure_threshold: usize,
    failure_window: Duration,
}

impl RestartPolicy {
    pub fn new(config: &WorkerConfig) -> RestartPolicy {
        let history = VecDeque::new();

        let initial_backoff = config.initial_backoff;
        let max_backoff = config.max_backoff;
        let failure_threshold = config.failure_threshold as usize;
        let failure_window = config.failure_window;

        Self {
            history,
            initial_backoff,
            max_backoff,
            failure_threshold,
            failure_window,
        }
    }

    pub fn record_failure(&mut self, now: Instant) -> FailureOutcome {
        self.history.push_back(now);

        while let Some(&oldest) = self.history.front() {
            if now - oldest > self.failure_window {
                self.history.pop_front();
            } else {
                break;
            }
        }

        if self.history.len() >= self.failure_threshold {
            return FailureOutcome::Permanent;
        }

        let mut backoff = self.initial_backoff;
        for _ in 1..self.history.len() {
            backoff = std::cmp::min(backoff * 2, self.max_backoff);
        }

        FailureOutcome::Backoff(backoff)
    }

    pub fn record_healthy_run(&mut self) {
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkerConfig;
    use std::time::{Duration, Instant};

    /// Build a RestartPolicy with known, easy-to-reason-about values.
    /// Using tiny numbers (ms rather than seconds) so tests stay fast
    /// and the math is obvious.
    fn test_config() -> WorkerConfig {
        let mut config = WorkerConfig::new("bun", "./script.ts", "/tmp/s.sock", "test");
        config.initial_backoff = Duration::from_millis(100);
        config.max_backoff = Duration::from_millis(1600);
        config.failure_threshold = 5;
        config.failure_window = Duration::from_secs(60);
        config.healthy_reset_duration = Duration::from_secs(10);
        config
    }

    #[test]
    fn first_failure_returns_initial_backoff() {
        let config = test_config();
        let mut policy = RestartPolicy::new(&config);
        let now = Instant::now();

        let outcome = policy.record_failure(now);

        assert_eq!(outcome, FailureOutcome::Backoff(Duration::from_millis(100)));
    }

    #[test]
    fn subsequent_failures_double_the_backoff() {
        let config = test_config();
        let mut policy = RestartPolicy::new(&config);
        let now = Instant::now();

        // Record failures in quick succession; each should double.
        assert_eq!(
            policy.record_failure(now),
            FailureOutcome::Backoff(Duration::from_millis(100))
        );
        assert_eq!(
            policy.record_failure(now),
            FailureOutcome::Backoff(Duration::from_millis(200))
        );
        assert_eq!(
            policy.record_failure(now),
            FailureOutcome::Backoff(Duration::from_millis(400))
        );
        assert_eq!(
            policy.record_failure(now),
            FailureOutcome::Backoff(Duration::from_millis(800))
        );
    }

    #[test]
    fn backoff_caps_at_max_backoff() {
        let mut config = test_config();
        // Lower the threshold so we can exercise the cap without
        // tripping the permanent-failure check.
        config.failure_threshold = 100;
        let mut policy = RestartPolicy::new(&config);
        let now = Instant::now();

        // With initial=100ms and max=1600ms, doubling hits cap at failure 5:
        // f1=100, f2=200, f3=400, f4=800, f5=1600 (at cap), f6=1600, ...
        for _ in 0..4 {
            policy.record_failure(now);
        }

        // Fifth failure reaches the cap.
        assert_eq!(
            policy.record_failure(now),
            FailureOutcome::Backoff(Duration::from_millis(1600))
        );

        // Subsequent failures stay at the cap, not beyond.
        assert_eq!(
            policy.record_failure(now),
            FailureOutcome::Backoff(Duration::from_millis(1600))
        );
        assert_eq!(
            policy.record_failure(now),
            FailureOutcome::Backoff(Duration::from_millis(1600))
        );
    }

    #[test]
    fn exceeding_threshold_returns_permanent() {
        let config = test_config(); // threshold = 5
        let mut policy = RestartPolicy::new(&config);
        let now = Instant::now();

        // First 4 failures should return Backoff.
        for _ in 0..4 {
            assert!(matches!(
                policy.record_failure(now),
                FailureOutcome::Backoff(_)
            ));
        }

        // Fifth failure hits the threshold.
        assert_eq!(policy.record_failure(now), FailureOutcome::Permanent);
    }

    #[test]
    fn failures_outside_window_are_forgotten() {
        let config = test_config(); // window = 60s, threshold = 5
        let mut policy = RestartPolicy::new(&config);
        let base = Instant::now();

        // Record 4 failures at t=0. This brings us one away from the threshold.
        for _ in 0..4 {
            policy.record_failure(base);
        }

        // Now record a failure at t=120s (well outside the 60s window).
        // The 4 old failures should be expired, leaving only this one.
        // So this should be treated as a fresh failure, returning initial backoff.
        let much_later = base + Duration::from_secs(120);
        assert_eq!(
            policy.record_failure(much_later),
            FailureOutcome::Backoff(Duration::from_millis(100))
        );
    }

    #[test]
    fn failures_inside_window_accumulate() {
        let config = test_config(); // window = 60s, threshold = 5
        let mut policy = RestartPolicy::new(&config);
        let base = Instant::now();

        // Record 4 failures at t=0, t=10s, t=20s, t=30s (all inside 60s window).
        policy.record_failure(base);
        policy.record_failure(base + Duration::from_secs(10));
        policy.record_failure(base + Duration::from_secs(20));
        policy.record_failure(base + Duration::from_secs(30));

        // 5th failure at t=40s — still inside window, should hit threshold.
        assert_eq!(
            policy.record_failure(base + Duration::from_secs(40)),
            FailureOutcome::Permanent
        );
    }

    #[test]
    fn partial_window_expiration() {
        let config = test_config(); // window = 60s, threshold = 5
        let mut policy = RestartPolicy::new(&config);
        let base = Instant::now();

        // Record failures at t=0 (will expire), t=10 (will expire),
        // t=65 (stays), t=70 (stays).
        policy.record_failure(base);
        policy.record_failure(base + Duration::from_secs(10));
        policy.record_failure(base + Duration::from_secs(65));
        policy.record_failure(base + Duration::from_secs(70));

        // Now record a failure at t=71. The window extends from t=11 to t=71.
        // So failures at t=0 and t=10 should have expired. We have 3 remaining
        // (t=65, t=70, t=71), well under threshold. Should return a backoff.
        let outcome = policy.record_failure(base + Duration::from_secs(71));
        assert!(
            matches!(outcome, FailureOutcome::Backoff(_)),
            "expected Backoff, got {:?}",
            outcome
        );
    }

    #[test]
    fn record_healthy_run_resets_history() {
        let config = test_config();
        let mut policy = RestartPolicy::new(&config);
        let now = Instant::now();

        // Record several failures to build up state.
        policy.record_failure(now);
        policy.record_failure(now);
        policy.record_failure(now);

        // Reset.
        policy.record_healthy_run();

        // Next failure should behave like the first — initial backoff.
        assert_eq!(
            policy.record_failure(now),
            FailureOutcome::Backoff(Duration::from_millis(100))
        );
    }

    #[test]
    fn reset_restores_ability_to_fail_up_to_threshold_again() {
        let config = test_config(); // threshold = 5
        let mut policy = RestartPolicy::new(&config);
        let now = Instant::now();

        // Fail 4 times, then reset.
        for _ in 0..4 {
            policy.record_failure(now);
        }
        policy.record_healthy_run();

        // After reset, should be able to fail 4 more times without going permanent.
        for _ in 0..4 {
            assert!(matches!(
                policy.record_failure(now),
                FailureOutcome::Backoff(_)
            ));
        }

        // The 5th failure post-reset hits the threshold.
        assert_eq!(policy.record_failure(now), FailureOutcome::Permanent);
    }

    #[test]
    fn threshold_of_one_fails_immediately() {
        let mut config = test_config();
        config.failure_threshold = 1;
        let mut policy = RestartPolicy::new(&config);
        let now = Instant::now();

        // With threshold = 1, the very first failure should go permanent.
        assert_eq!(policy.record_failure(now), FailureOutcome::Permanent);
    }

    #[test]
    fn initial_equals_max_means_constant_backoff() {
        let mut config = test_config();
        config.initial_backoff = Duration::from_millis(500);
        config.max_backoff = Duration::from_millis(500);
        config.failure_threshold = 100; // high enough not to trip
        let mut policy = RestartPolicy::new(&config);
        let now = Instant::now();

        // Every failure should return the same backoff (no growth possible).
        for _ in 0..5 {
            assert_eq!(
                policy.record_failure(now),
                FailureOutcome::Backoff(Duration::from_millis(500))
            );
        }
    }
}
