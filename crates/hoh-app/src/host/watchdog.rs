//! The inactivity watchdog of an attempt the runner runs (docs/adr/ADR-022.md decision 19, REQ-042 behaviour 8): the
//! time since the attempt's last output, on its own monotonic clock, apart from any wall deadline. An attempt is
//! inactive once it has had neither output nor a tool process for [`Watchdog::quiet`]. It never kills: one event per
//! episode, cleared by the next output.

use super::processes::Tools;
use std::time::{Duration, Instant};

/// The watchdog's timing: a host constant, not a configuration key (decision 19 item 5); tests shorten it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watchdog {
    /// How long an attempt goes without output and without a tool process before it is inactive (15 min).
    pub quiet: Duration,
    /// How often it is looked at (15 s).
    pub every: Duration,
}

impl Default for Watchdog {
    fn default() -> Self {
        Watchdog {
            quiet: Duration::from_secs(15 * 60),
            every: Duration::from_secs(15),
        }
    }
}

/// What a look found, to be recorded as an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Change {
    /// Episode `episode` begins: no output for `since`, and no tool process for the quiet time.
    Inactive { episode: u32, since: Duration },
    /// Episode `episode` ends: output was seen `after` it began.
    Cleared { episode: u32, after: Duration },
}

/// One attempt's watch, from its gate's opening.
#[derive(Debug, Clone)]
pub(super) struct Watch {
    quiet: Duration,
    /// Its output's size at the last look that could tell: nothing before its gate opened.
    bytes: u64,
    /// When a look last saw its output grow (its gate's opening before any).
    output: Instant,
    /// The later of [`Watch::output`] and the last look that found a tool process or could not tell.
    active: Instant,
    /// The episodes recorded so far.
    episode: u32,
    /// When the episode recorded last began, while it is open.
    open: Option<Instant>,
}

impl Watch {
    /// A watch from `at`, when its gate opened.
    pub(super) fn new(quiet: Duration, at: Instant) -> Watch {
        Watch {
            quiet,
            bytes: 0,
            output: at,
            active: at,
            episode: 0,
            open: None,
        }
    }

    /// A look at `at`: `bytes` its output so far (`None`: cannot tell), `tools` asked only when it had no output
    /// since the last look and no episode is open. The change to record, if any; it takes effect once
    /// [`Watch::recorded`].
    pub(super) fn look(
        &mut self,
        at: Instant,
        bytes: Option<u64>,
        tools: impl FnOnce() -> Tools,
    ) -> Option<Change> {
        let grew = bytes.is_some_and(|b| std::mem::replace(&mut self.bytes, b) != b);
        if grew {
            self.output = at;
            self.active = at;
        }
        if let Some(began) = self.open {
            // Open until output is seen; nothing else is looked at meanwhile.
            return (self.output > began).then(|| Change::Cleared {
                episode: self.episode,
                after: self.output.saturating_duration_since(began),
            });
        }
        // Output that cannot be told, a tool running or tools that cannot be told: as active as output.
        if bytes.is_none() || (!grew && tools() != Tools::Absent) {
            self.active = at;
        }
        (at.saturating_duration_since(self.active) >= self.quiet).then(|| Change::Inactive {
            episode: self.episode + 1,
            since: at.saturating_duration_since(self.output),
        })
    }

    /// `change`, found by the look at `at`, was recorded.
    pub(super) fn recorded(&mut self, change: Change, at: Instant) {
        match change {
            Change::Inactive { episode, .. } => {
                self.episode = episode;
                self.open = Some(at);
            }
            Change::Cleared { .. } => self.open = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Duration = Duration::from_secs(60);

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// A tools answer the look must not ask for.
    fn unasked() -> Tools {
        panic!("the tools were asked for")
    }

    #[test]
    fn the_watchdog_waits_fifteen_minutes_and_looks_every_fifteen_seconds() {
        let w = Watchdog::default();
        assert_eq!((w.quiet, w.every), (s(15 * 60), s(15)));
    }

    #[test]
    fn an_attempt_whose_output_grows_is_never_inactive_and_its_tools_are_not_asked_for() {
        let t0 = Instant::now();
        let mut w = Watch::new(QUIET, t0);
        for i in 1..=20u64 {
            assert_eq!(w.look(t0 + s(30 * i), Some(i), unasked), None, "look {i}");
        }
    }

    #[test]
    fn a_silent_attempt_with_a_tool_running_is_not_inactive_however_long() {
        let t0 = Instant::now();
        let mut w = Watch::new(QUIET, t0);
        for i in 1..=40u64 {
            assert_eq!(
                w.look(t0 + s(15 * i), Some(0), || Tools::Running),
                None,
                "look {i}"
            );
        }
    }

    #[test]
    fn a_silent_attempt_whose_tools_or_output_cannot_be_told_is_not_inactive() {
        let t0 = Instant::now();
        let mut unknown = Watch::new(QUIET, t0);
        let mut unread = Watch::new(QUIET, t0);
        for i in 1..=40u64 {
            assert_eq!(
                unknown.look(t0 + s(15 * i), Some(0), || Tools::Unknown),
                None
            );
            assert_eq!(unread.look(t0 + s(15 * i), None, unasked), None);
        }
    }

    #[test]
    fn silence_with_no_tool_for_the_quiet_time_is_one_inactive_episode() {
        let t0 = Instant::now();
        let mut w = Watch::new(QUIET, t0);
        assert_eq!(w.look(t0 + s(1), Some(10), unasked), None);
        assert_eq!(w.look(t0 + s(31), Some(10), || Tools::Absent), None);
        assert_eq!(w.look(t0 + s(60), Some(10), || Tools::Absent), None);
        let inactive = Change::Inactive {
            episode: 1,
            since: s(60),
        };
        assert_eq!(
            w.look(t0 + s(61), Some(10), || Tools::Absent),
            Some(inactive)
        );
        w.recorded(inactive, t0 + s(61));
        // Once per episode: later silent looks find nothing, and do not scan for tools.
        for i in 1..=20u64 {
            assert_eq!(w.look(t0 + s(61 + 15 * i), Some(10), unasked), None);
        }
    }

    #[test]
    fn the_quiet_time_counts_from_the_last_look_that_found_a_tool() {
        let t0 = Instant::now();
        let mut w = Watch::new(QUIET, t0);
        assert_eq!(w.look(t0 + s(1), Some(10), unasked), None);
        for at in [16, 31, 46] {
            assert_eq!(w.look(t0 + s(at), Some(10), || Tools::Running), None);
        }
        // The tool is gone: 60 s from its last sighting at 46, not from the output at 1.
        for at in [61, 76, 91] {
            assert_eq!(
                w.look(t0 + s(at), Some(10), || Tools::Absent),
                None,
                "at {at}"
            );
        }
        assert_eq!(
            w.look(t0 + s(106), Some(10), || Tools::Absent),
            Some(Change::Inactive {
                episode: 1,
                since: s(105)
            })
        );
    }

    #[test]
    fn output_clears_the_episode_and_the_next_one_needs_the_quiet_time_again() {
        let t0 = Instant::now();
        let mut w = Watch::new(QUIET, t0);
        let first = w.look(t0 + s(60), Some(0), || Tools::Absent);
        assert_eq!(
            first,
            Some(Change::Inactive {
                episode: 1,
                since: s(60)
            })
        );
        w.recorded(
            Change::Inactive {
                episode: 1,
                since: s(60),
            },
            t0 + s(60),
        );
        let cleared = w.look(t0 + s(75), Some(5), unasked);
        assert_eq!(
            cleared,
            Some(Change::Cleared {
                episode: 1,
                after: s(15)
            })
        );
        w.recorded(
            Change::Cleared {
                episode: 1,
                after: s(15),
            },
            t0 + s(75),
        );
        assert_eq!(w.look(t0 + s(90), Some(5), || Tools::Absent), None);
        assert_eq!(w.look(t0 + s(134), Some(5), || Tools::Absent), None);
        assert_eq!(
            w.look(t0 + s(135), Some(5), || Tools::Absent),
            Some(Change::Inactive {
                episode: 2,
                since: s(60)
            })
        );
    }

    #[test]
    fn a_change_that_was_not_recorded_is_found_again_by_the_next_look() {
        let t0 = Instant::now();
        let mut w = Watch::new(QUIET, t0);
        assert_eq!(
            w.look(t0 + s(60), Some(0), || Tools::Absent),
            Some(Change::Inactive {
                episode: 1,
                since: s(60)
            })
        );
        // Its event was not written: the next look proposes the same episode.
        assert_eq!(
            w.look(t0 + s(75), Some(0), || Tools::Absent),
            Some(Change::Inactive {
                episode: 1,
                since: s(75)
            })
        );
        w.recorded(
            Change::Inactive {
                episode: 1,
                since: s(75),
            },
            t0 + s(75),
        );
        assert_eq!(
            w.look(t0 + s(90), Some(3), unasked),
            Some(Change::Cleared {
                episode: 1,
                after: s(15)
            })
        );
        // Not written either: still open, cleared again however the output stands.
        assert_eq!(
            w.look(t0 + s(105), Some(3), unasked),
            Some(Change::Cleared {
                episode: 1,
                after: s(15)
            })
        );
    }

    #[test]
    fn a_look_that_cannot_read_the_output_neither_clears_nor_counts_as_silence() {
        let t0 = Instant::now();
        let mut w = Watch::new(QUIET, t0);
        w.recorded(
            Change::Inactive {
                episode: 1,
                since: s(60),
            },
            t0 + s(60),
        );
        assert_eq!(w.look(t0 + s(75), None, unasked), None);
        assert_eq!(w.look(t0 + s(90), Some(0), unasked), None);
    }
}
