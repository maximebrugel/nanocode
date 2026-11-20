use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    interval: Duration,
    last: Option<Duration>,
}

impl Schedule {
    pub const fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    pub fn due(&self, now: Duration) -> bool {
        self.last
            .is_none_or(|last| now.saturating_sub(last) >= self.interval)
    }

    pub const fn ran(&mut self, now: Duration) {
        self.last = Some(now);
    }
}
