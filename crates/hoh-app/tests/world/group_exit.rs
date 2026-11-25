//! The group-exit barrier of a scripted executor (FLK-11, M3FK-L1): reaping a fixture's group leader can precede the
//! kernel's release of its process group, so one `group_empty` sample right after the reap can still read false under
//! load. A false `Outcome::group_gone` keeps the attempt RUNNING with `runner.unreaped`, and a test waiting for its end
//! then fails. Fixtures report the group only after this barrier.

use std::time::{Duration, Instant};

/// How long a fixture observes its reaped leader's group (`Team::review`'s bound). It limits that cleanup observation
/// only: a group still alive after it is reported as not gone, never as gone.
pub const GROUP_EXIT_BOUND: Duration = Duration::from_secs(5);

/// Whether process group `pgid`, whose leader the caller has reaped, is empty (`identity::group_empty`), looked at
/// every 10 ms until it is or [`GROUP_EXIT_BOUND`] has passed.
pub fn group_gone_after_reap(pgid: u32) -> bool {
    let until = Instant::now() + GROUP_EXIT_BOUND;
    let mut gone = hoh_agents::identity::group_empty(pgid);
    while !gone && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
        gone = hoh_agents::identity::group_empty(pgid);
    }
    gone
}
