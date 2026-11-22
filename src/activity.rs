//! Shared presentation of work owned by the current view.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    GithubFile,
    RangeDiff,
    PullRequests,
    PatchMatches,
    CodeStats,
    History,
    Reflog,
    BranchConnections,
    Images,
    Browser,
}

impl Task {
    fn label(self) -> &'static str {
        match self {
            Self::GithubFile => "Loading GitHub file…",
            Self::RangeDiff => "Loading range-diff…",
            Self::PullRequests => "Loading PRs…",
            Self::PatchMatches => "Comparing patches…",
            Self::CodeStats => "Counting code…",
            Self::Reflog => "Loading full reflog…",
            Self::History => "Reading history…",
            Self::BranchConnections => "Reading branch connections…",
            Self::Images => "Loading images…",
            Self::Browser => "Opening browser…",
        }
    }
}

#[derive(Default)]
pub struct Indicator {
    since: Option<Instant>,
    text: Option<String>,
}

impl Indicator {
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// The caller supplies actual pending work rather than maintaining a
    /// second registry of starts/stops. Fast tasks never flash the header.
    pub fn poll(&mut self, tasks: &[Task], now: Instant) -> bool {
        let text = if let Some(task) = tasks.first() {
            let since = *self.since.get_or_insert(now);
            let elapsed = now.saturating_duration_since(since);
            if elapsed < Duration::from_millis(150) {
                None
            } else {
                let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
                let frame = frames[(elapsed.as_millis() / 250) as usize % frames.len()];
                let extra = if tasks.len() > 1 {
                    format!(" +{}", tasks.len() - 1)
                } else {
                    String::new()
                };
                Some(format!(" {frame} {}{extra} ", task.label()))
            }
        } else {
            self.since = None;
            None
        };
        let changed = self.text != text;
        self.text = text;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_tasks_stay_quiet_and_completion_clears_the_animation() {
        let now = Instant::now();
        let mut indicator = Indicator::default();
        assert!(!indicator.poll(&[Task::PullRequests], now));
        assert!(!indicator.poll(&[], now + Duration::from_millis(100)));
        assert!(!indicator.poll(&[Task::GithubFile], now + Duration::from_secs(1)));
        assert!(indicator.poll(
            &[Task::GithubFile, Task::CodeStats],
            now + Duration::from_millis(1200)
        ));
        assert!(indicator
            .text()
            .unwrap()
            .contains("Loading GitHub file… +1"));
        let first = indicator.text().unwrap().to_owned();
        assert!(indicator.poll(
            &[Task::GithubFile, Task::CodeStats],
            now + Duration::from_millis(1500)
        ));
        assert_ne!(indicator.text().unwrap(), first);
        assert!(indicator.poll(&[Task::CodeStats], now + Duration::from_millis(1501)));
        assert!(indicator.text().unwrap().contains("Counting code…"));
        assert!(indicator.poll(&[], now + Duration::from_secs(2)));
        assert!(indicator.text().is_none());
        assert!(!indicator.poll(&[], now + Duration::from_secs(3)));
    }
}
