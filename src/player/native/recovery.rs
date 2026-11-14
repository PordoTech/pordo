use std::time::{Duration, Instant};

/// Each device outage owns its retry schedule. Successful switches never spend
/// a lifetime budget; after the fast window, retry quietly at a lower rate.
#[derive(Default)]
pub(super) struct Recovery {
    due: Option<Instant>,
    deadline: Option<Instant>,
    warned: bool,
}
impl Recovery {
    pub fn initial(pending: bool, now: Instant) -> Self {
        Self {
            due: pending.then_some(now),
            ..Default::default()
        }
    }
    pub fn disconnected(&mut self, now: Instant) {
        *self = Self {
            due: Some(now + Duration::from_millis(200)),
            ..Default::default()
        };
    }
    pub fn ready(&mut self, now: Instant, active: bool) -> bool {
        if !active {
            // Pausing video does not spend the fast-retry window.
            self.deadline = None;
            return false;
        }
        if self.due.is_some_and(|due| now >= due) {
            self.deadline.get_or_insert(now + Duration::from_secs(10));
            self.due = None;
            true
        } else {
            false
        }
    }
    /// True once per prolonged outage, for a nonfatal user notice.
    pub fn failed(&mut self, now: Instant) -> bool {
        let warn = !self.warned && self.deadline.is_some_and(|at| now >= at);
        self.warned |= warn;
        self.due = Some(
            now + if self.warned {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(500)
            },
        );
        warn
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn waiting(&self) -> bool {
        self.due.is_some() || self.deadline.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_recovery_keeps_retrying_after_grace_and_repeated_successful_switches() {
        let mut now = Instant::now();
        let mut recovery = Recovery::default();
        for _ in 0..16 {
            recovery.disconnected(now);
            assert!(!recovery.ready(now, true));
            now += Duration::from_millis(200);
            assert!(recovery.ready(now, true));
            recovery.clear();
            assert!(!recovery.waiting());
        }
        recovery.disconnected(now);
        now += Duration::from_millis(200);
        assert!(recovery.ready(now, true));
        now += Duration::from_secs(10);
        assert!(recovery.failed(now));
        assert!(!recovery.ready(now + Duration::from_secs(1), true));
        now += Duration::from_secs(2);
        assert!(recovery.ready(now, true));
        assert!(
            !recovery.failed(now),
            "one notice per outage, no terminal error"
        );
        now += Duration::from_secs(2);
        assert!(recovery.ready(now, true));
    }
    #[test]
    fn device_recovery_pausing_does_not_exhaust_the_retry_window() {
        let now = Instant::now();
        let mut recovery = Recovery::initial(true, now);
        assert!(recovery.ready(now, true));
        assert!(!recovery.failed(now));
        assert!(!recovery.ready(now + Duration::from_secs(100), false));
        assert!(recovery.ready(now + Duration::from_secs(101), true));
        assert!(!recovery.failed(now + Duration::from_secs(101)));
        assert!(recovery.waiting());
    }
}
