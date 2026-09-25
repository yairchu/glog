//! Merge folds keep the full history available for search and patch loading.
//! Only the visible graph and navigation are projected onto the unfolded commits.
use std::collections::{HashMap, HashSet};

use crate::git::{self, Commit, CommitKind};

#[derive(Default)]
pub struct LogFolds {
    pub start_collapsed: bool,
    seen: HashSet<String>,
    collapsed: HashSet<String>,
    members: HashMap<String, HashSet<String>>,
    hidden: HashSet<usize>,
    graphs: HashMap<usize, Vec<String>>,
    counts: HashMap<String, usize>,
    ancestry: HashMap<String, Vec<String>>,
    ancestry_for: Vec<(String, Vec<String>)>,
    graph_ready: bool,
}

impl LogFolds {
    pub fn visible(&self, index: usize) -> bool {
        !self.hidden.contains(&index)
    }

    pub fn graph<'a>(&'a self, index: usize, commit: &'a Commit) -> &'a [String] {
        self.graphs.get(&index).unwrap_or(&commit.graph)
    }

    pub fn marker(&self, commit: &Commit) -> Option<&'static str> {
        if commit.parents.len() < 2 {
            return None;
        }
        if self.graph_ready && self.collapsed.contains(&commit.hash) {
            Some("▶")
        } else {
            Some("▼")
        }
    }

    pub fn label(&self, commit: &Commit) -> String {
        if self.marker(commit) == Some("▶") {
            let count = self.counts.get(&commit.hash).copied().unwrap_or(0);
            format!(
                " · {count} merged commit{}",
                if count == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        }
    }

    pub fn refresh(&mut self, commits: &[Commit]) -> Result<(), String> {
        let hashes: HashSet<_> = commits.iter().map(|c| c.hash.clone()).collect();
        self.collapsed.retain(|hash| hashes.contains(hash));
        self.members.retain(|hash, _| hashes.contains(hash));
        self.seen.retain(|hash| hashes.contains(hash));
        let result = (|| {
            for commit in commits {
                if self.start_collapsed
                    && !self.seen.contains(&commit.hash)
                    && commit.parents.len() > 1
                {
                    self.load_members(commit)?;
                    if self.members[&commit.hash]
                        .iter()
                        .any(|hash| hashes.contains(hash))
                    {
                        self.collapsed.insert(commit.hash.clone());
                    }
                }
                self.seen.insert(commit.hash.clone());
            }
            Ok(())
        })();
        // Even on an I/O error, indices must refer to the new commit list.
        let graph = self.prepare_graph(commits);
        self.graph_ready = graph.is_ok();
        self.rebuild(commits);
        result.and(graph)
    }

    fn load_members(&mut self, commit: &Commit) -> Result<(), String> {
        if !self.members.contains_key(&commit.hash) {
            self.members
                .insert(commit.hash.clone(), git::merged_commits(&commit.hash)?);
        }
        Ok(())
    }

    pub fn toggle(&mut self, index: usize, commits: &[Commit]) -> Result<(), String> {
        let Some(commit) = commits.get(index).filter(|c| c.parents.len() > 1) else {
            return Ok(());
        };
        let previous = self.collapsed.clone();
        if !self.collapsed.remove(&commit.hash) {
            self.load_members(commit)?;
            if !commits
                .iter()
                .any(|c| self.members[&commit.hash].contains(&c.hash))
            {
                return Err(
                    "No merged commits in this history; revision and path filters still apply"
                        .into(),
                );
            }
            self.collapsed.insert(commit.hash.clone());
        }
        if let Err(error) = self.prepare_graph(commits) {
            self.collapsed = previous;
            return Err(error);
        }
        self.graph_ready = true;
        self.rebuild(commits);
        Ok(())
    }

    pub fn toggle_all(&mut self, selected: usize, commits: &[Commit]) -> Result<usize, String> {
        let loaded: HashSet<_> = commits.iter().map(|c| &c.hash).collect();
        let mut eligible = HashSet::new();
        for commit in commits.iter().filter(|c| c.parents.len() > 1) {
            self.load_members(commit)?;
            if self.members[&commit.hash]
                .iter()
                .any(|hash| loaded.contains(hash))
            {
                eligible.insert(commit.hash.clone());
            }
        }
        let next = if eligible.is_subset(&self.collapsed) {
            HashSet::new()
        } else {
            eligible
        };
        let previous = std::mem::replace(&mut self.collapsed, next);
        if let Err(error) = self.prepare_graph(commits) {
            self.collapsed = previous;
            return Err(error);
        }
        self.graph_ready = true;
        self.rebuild(commits);
        if !self.visible(selected) {
            if let Some(commit) = commits.get(selected) {
                if let Some(owner) = (0..commits.len()).rev().find(|&i| {
                    self.visible(i)
                        && self.collapsed.contains(&commits[i].hash)
                        && self.members[&commits[i].hash].contains(&commit.hash)
                }) {
                    return Ok(owner);
                }
            }
        }
        Ok(selected)
    }

    fn prepare_graph(&mut self, commits: &[Commit]) -> Result<(), String> {
        if self.collapsed.is_empty() {
            return Ok(());
        }
        let key: Vec<_> = commits
            .iter()
            .filter(|c| c.kind == CommitKind::Revision)
            .map(|c| (c.hash.clone(), c.parents.clone()))
            .collect();
        if key == self.ancestry_for {
            return Ok(());
        }
        let loaded: HashSet<_> = key.iter().map(|(hash, _)| hash).collect();
        let boundary: HashSet<_> = key
            .last()
            .into_iter()
            .flat_map(|(_, parents)| parents)
            .collect();
        let missing = key
            .iter()
            .flat_map(|(_, parents)| parents)
            .any(|p| !loaded.contains(p) && !boundary.contains(p));
        let ancestry = if missing {
            git::log_ancestry(commits)?
        } else {
            HashMap::new()
        };
        self.ancestry = ancestry;
        self.ancestry_for = key;
        Ok(())
    }

    pub fn reveal(&mut self, index: usize, commits: &[Commit]) {
        let Some(commit) = commits.get(index) else {
            return;
        };
        if self.visible(index) {
            return;
        }
        self.collapsed.retain(|hash| {
            !self
                .members
                .get(hash)
                .is_some_and(|members| members.contains(&commit.hash))
        });
        self.rebuild(commits);
    }

    fn rebuild(&mut self, commits: &[Commit]) {
        self.hidden.clear();
        self.graphs.clear();
        if !self.graph_ready {
            return;
        }
        let loaded: HashSet<_> = commits.iter().map(|c| &c.hash).collect();
        self.counts = self
            .collapsed
            .iter()
            .map(|hash| {
                let count = self.members.get(hash).map_or(0, |members| {
                    members
                        .iter()
                        .filter(|member| loaded.contains(member))
                        .count()
                });
                (hash.clone(), count)
            })
            .collect();
        let hidden_hashes: HashSet<_> = self
            .collapsed
            .iter()
            .filter_map(|hash| self.members.get(hash))
            .flatten()
            .collect();
        self.hidden = commits
            .iter()
            .enumerate()
            .filter(|(_, c)| hidden_hashes.contains(&c.hash))
            .map(|(index, _)| index)
            .collect();
        self.graphs.clear();
        if self.hidden.is_empty() {
            return;
        }
        let by_hash: HashMap<_, _> = commits
            .iter()
            .enumerate()
            .map(|(index, c)| (c.hash.as_str(), index))
            .collect();
        let mut lanes = Vec::<String>::new();
        let mut pending = Vec::new();
        for (index, commit) in commits.iter().enumerate() {
            if !self.visible(index) {
                continue;
            }
            if commit.kind != CommitKind::Revision {
                self.graphs.insert(index, commit.graph.clone());
                continue;
            }
            let column = lanes
                .iter()
                .position(|hash| hash == &commit.hash)
                .unwrap_or_else(|| {
                    lanes.push(commit.hash.clone());
                    lanes.len() - 1
                });
            let node: String = (0..lanes.len())
                .map(|i| if i == column { "* " } else { "| " })
                .collect();
            pending.push(node);
            self.graphs.insert(index, std::mem::take(&mut pending));

            // A collapsed merge follows its first parent. Other edges bypass
            // hidden nodes, preserving connections from independently visible branches.
            let parents = if self.collapsed.contains(&commit.hash) {
                &commit.parents[..commit.parents.len().min(1)]
            } else {
                &commit.parents
            };
            let mut projected = Vec::new();
            let mut todo: Vec<_> = parents.iter().rev().collect();
            let mut visited = HashSet::new();
            while let Some(parent) = todo.pop() {
                if !visited.insert(parent) {
                    continue;
                }
                let Some(&parent_index) = by_hash.get(parent.as_str()) else {
                    if let Some(parents) = self.ancestry.get(parent) {
                        todo.extend(parents.iter().rev());
                    }
                    continue;
                };
                if self.visible(parent_index) {
                    projected.push(parent.clone());
                } else {
                    todo.extend(commits[parent_index].parents.iter().rev());
                }
            }
            let mut next = lanes.clone();
            next.remove(column);
            let mut insert = column;
            for parent in &projected {
                if !next.contains(parent) {
                    next.insert(insert, parent.clone());
                    insert += 1;
                }
            }
            let mut edges = Vec::new();
            for (from, hash) in lanes.iter().enumerate() {
                let targets = if from == column {
                    &projected[..]
                } else {
                    std::slice::from_ref(hash)
                };
                for target in targets {
                    if let Some(to) = next.iter().position(|h| h == target) {
                        edges.push((2 * from, 2 * to));
                    }
                }
            }
            pending = transitions(&edges, lanes.len().max(next.len()));
            lanes = next;
        }
    }
}

// Route edges a character at a time. Crossings retain their independent targets;
// '+' joins routes to the same target, 'X' crosses routes to different targets.
fn transitions(edges: &[(usize, usize)], width: usize) -> Vec<String> {
    let distance = edges.iter().map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    let mut rows = Vec::new();
    for step in 1..=distance {
        let mut cells = vec![' '; width * 2];
        let mut targets = vec![None; width * 2];
        for &(from, to) in edges {
            let shift = step.min(from.abs_diff(to));
            let position = if from < to {
                from + shift
            } else {
                from - shift
            };
            let symbol = if step > from.abs_diff(to) {
                '|'
            } else if from < to {
                '\\'
            } else {
                '/'
            };
            cells[position] = if cells[position] == ' ' {
                symbol
            } else if targets[position] == Some(to) {
                '+'
            } else {
                'X'
            };
            targets[position] = Some(to);
        }
        rows.push(format!("{} ", cells.iter().collect::<String>().trim_end()));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(hash: &str, parents: &[&str]) -> Commit {
        let mut commit = crate::log_format::tests::commit();
        commit.hash = hash.into();
        commit.parents = parents.iter().map(|p| (*p).into()).collect();
        commit.graph = vec![format!("original graph {hash}")];
        commit
    }

    #[test]
    fn folded_merge_reconnects_interleaved_lanes_and_restores_original_graph() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("unrelated", &["base"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds
            .members
            .insert("merge".into(), HashSet::from(["side".into()]));
        folds.toggle(0, &commits).unwrap();
        assert!(!folds.visible(2));
        assert_eq!(folds.graph(0, &commits[0]), ["* "]);
        assert_eq!(folds.graph(1, &commits[1]), ["| * "]);
        assert_eq!(folds.graph(3, &commits[3]), ["* | "]);
        assert_eq!(folds.graph(4, &commits[4]), ["|/ ", "+ ", "* "]);
        folds.toggle(0, &commits).unwrap();
        for (i, commit) in commits.iter().enumerate() {
            assert!(folds.visible(i));
            assert_eq!(folds.graph(i, commit), commit.graph);
        }
    }

    #[test]
    fn revealing_shared_history_opens_all_owners_but_keeps_other_folds() {
        let commits = vec![
            commit("one", &["base", "shared"]),
            commit("two", &["base", "shared"]),
            commit("three", &["base", "other"]),
            commit("shared", &["base"]),
            commit("other", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        for owner in ["one", "two"] {
            folds
                .members
                .insert(owner.into(), HashSet::from(["shared".into()]));
        }
        folds
            .members
            .insert("three".into(), HashSet::from(["other".into()]));
        for i in 0..3 {
            folds.toggle(i, &commits).unwrap();
        }
        folds.reveal(3, &commits);
        assert!(folds.visible(3));
        assert!(!folds.visible(4));
        assert_eq!(folds.collapsed, HashSet::from(["three".into()]));
    }

    #[test]
    fn crossing_routes_do_not_turn_into_a_shared_parent() {
        assert_eq!(transitions(&[(0, 2), (2, 0)], 2), [" X ", "/ \\ "]);
    }
}
