//! Share one topological ancestry walk across bulk merge-fold requests.
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet},
};

pub fn members(history: &str, merges: &[&str]) -> Result<HashMap<String, HashSet<String>>, String> {
    let rows: Vec<Vec<_>> = history
        .lines()
        .map(|line| line.split_whitespace().collect())
        .filter(|row: &Vec<&str>| !row.is_empty())
        .collect();
    let indices: HashMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row[0], index))
        .collect();
    let parents: Vec<Vec<usize>> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            row[1..]
                .iter()
                .map(|parent| {
                    indices
                        .get(parent)
                        .copied()
                        .filter(|&parent| parent > index)
                        .ok_or_else(|| "incomplete or unordered merge ancestry".to_owned())
                })
                .collect()
        })
        .collect::<Result<_, _>>()?;
    merges
        .iter()
        .map(|&hash| {
            let index = indices.get(hash).ok_or("merge missing from ancestry")?;
            Ok((
                hash.to_owned(),
                difference(&parents, *index)
                    .into_iter()
                    .map(|index| rows[index][0].to_owned())
                    .collect(),
            ))
        })
        .collect()
}

fn difference(parents: &[Vec<usize>], merge: usize) -> HashSet<usize> {
    // Bit 1 means reachable from the first parent, bit 2 from a side parent.
    // Process children before parents, so a node's flags are final when popped.
    // Stop once no exclusively-side frontier remains: older shared history
    // cannot contribute members, and need not be visited for every merge.
    let mut flags = HashMap::<usize, u8>::new();
    let mut todo = BinaryHeap::new();
    let mut side_pending = 0usize;
    let enqueue = |index,
                   mask,
                   flags: &mut HashMap<usize, u8>,
                   todo: &mut BinaryHeap<_>,
                   pending: &mut usize| {
        let previous = flags.get(&index).copied().unwrap_or(0);
        let next = previous | mask;
        if previous == 2 {
            *pending -= 1;
        }
        if next == 2 {
            *pending += 1;
        }
        if previous == 0 {
            todo.push(Reverse(index));
        }
        flags.insert(index, next);
    };
    for (i, &parent) in parents[merge].iter().enumerate() {
        enqueue(
            parent,
            if i == 0 { 1 } else { 2 },
            &mut flags,
            &mut todo,
            &mut side_pending,
        );
    }
    let mut result = HashSet::new();
    while side_pending > 0 {
        let Reverse(index) = todo.pop().unwrap();
        let mask = flags[&index];
        if mask == 2 {
            result.insert(index);
            side_pending -= 1;
        }
        for &parent in &parents[index] {
            enqueue(parent, mask, &mut flags, &mut todo, &mut side_pending);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontier_matches_full_reachability_with_shared_and_octopus_history() {
        fn reachable(parents: &[Vec<usize>], roots: &[usize]) -> HashSet<usize> {
            let mut found = HashSet::new();
            let mut todo = roots.to_vec();
            while let Some(index) = todo.pop() {
                if found.insert(index) {
                    todo.extend(&parents[index]);
                }
            }
            found
        }
        // Deterministic DAGs include unrelated roots, redundant parents,
        // criss-cross histories, and side branches merged more than once.
        let mut seed = 17u32;
        for _ in 0..100 {
            let mut parents = vec![Vec::new(); 40];
            for (index, row) in parents.iter_mut().enumerate() {
                for parent in index + 1..40 {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    if seed.is_multiple_of(7) {
                        row.push(parent);
                    }
                }
                if seed.is_multiple_of(2) {
                    row.reverse();
                }
            }
            for (index, row) in parents.iter().enumerate().filter(|(_, row)| row.len() > 1) {
                let main = reachable(&parents, &row[..1]);
                let side = reachable(&parents, &row[1..]);
                assert_eq!(difference(&parents, index), &side - &main);
            }
        }
    }

    #[test]
    fn bulk_handles_a_long_chain_of_short_branches() {
        let merges = 10_000;
        let mut parents = vec![Vec::new(); merges * 3 + 1];
        for i in 0..merges {
            let node = i * 3;
            parents[node] = vec![node + 1, node + 2];
            parents[node + 1] = vec![node + 3];
            parents[node + 2] = vec![node + 3];
        }
        for i in 0..merges {
            assert_eq!(difference(&parents, i * 3), HashSet::from([i * 3 + 2]));
        }
    }
}
