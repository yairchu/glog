//! Reachability between the targets of successive reflog rebases.
use std::collections::{HashMap, HashSet};

use super::{pipe_through, read_command_at, read_concurrently, Repository};

pub(crate) fn ancestor_pairs(pairs: &[(&str, &str)], repository: Option<&Repository>) -> Vec<bool> {
    let individually = |&(old, new): &(&str, &str)| {
        read_command_at(repository)
            .args(["merge-base", "--is-ancestor", old, new])
            .env("GIT_NO_LAZY_FETCH", "1")
            .output()
            .is_ok_and(|output| output.status.success())
    };
    // A single merge-base can stop as soon as Git finds the older target.
    if pairs.len() < 2 {
        return read_concurrently(pairs, individually);
    }
    let tips: HashSet<_> = pairs.iter().map(|&(_, new)| new).collect();
    let input: String = tips.into_iter().map(|tip| format!("{tip}\n")).collect();
    // Keep a small set of queries from loading an entire large repository.
    let limit = pairs.len().saturating_mul(256);
    let Some(output) = pipe_through(
        read_command_at(repository)
            .args(["rev-list", "--parents", "--topo-order", "--stdin"])
            .arg(format!("--max-count={limit}"))
            .env("GIT_NO_LAZY_FETCH", "1"),
        input.as_bytes(),
    ) else {
        // One unavailable object must not prevent folding unrelated rebases.
        return read_concurrently(pairs, individually);
    };

    // Build from roots to tips, so a parent always has a smaller index.
    let mut indices = HashMap::new();
    let mut parents = Vec::new();
    for line in output.lines().rev() {
        let mut fields = line.split_ascii_whitespace();
        let Some(hash) = fields.next() else { continue };
        indices.insert(hash, parents.len());
        parents.push(
            fields
                .filter_map(|hash| indices.get(hash).copied())
                .collect::<Vec<usize>>(),
        );
    }
    read_concurrently(pairs, |pair @ &(old, new)| {
        let Some(&old) = indices.get(old) else {
            return parents.len() == limit && individually(pair);
        };
        // Even a truncated walk contains every path from a newer target
        // down to an emitted ancestor, since children precede parents.
        let Some(&new) = indices.get(new) else {
            return false;
        };
        let mut pending = vec![new];
        let mut visited = HashSet::new();
        while let Some(index) = pending.pop() {
            if index == old {
                return true;
            }
            if index > old && visited.insert(index) {
                pending.extend(&parents[index]);
            }
        }
        false
    })
}
