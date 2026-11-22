//! Reachability between the targets of successive reflog rebases.
use super::{read_command_at, read_concurrently, Repository};

pub(crate) fn ancestor_pairs(pairs: &[(&str, &str)], repository: Option<&Repository>) -> Vec<bool> {
    // A topological rev-list can walk the full reachable history before
    // emitting even a bounded list. Merge-base can stop at each older target.
    read_concurrently(pairs, |&(old, new)| {
        read_command_at(repository)
            .args(["merge-base", "--is-ancestor", old, new])
            .env("GIT_NO_LAZY_FETCH", "1")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}
