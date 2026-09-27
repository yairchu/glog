//! Merge folds keep the full history available for search and patch loading.
//! Only the visible graph and navigation are projected onto the unfolded commits.
use std::collections::{HashMap, HashSet};

use crate::git::{self, Commit, CommitKind};

#[derive(Default)]
pub struct LogFolds {
    pub start_collapsed: bool,
    seen: HashSet<String>,
    collapsed: HashSet<String>,
    foldable: HashSet<String>,
    members: HashMap<String, HashSet<String>>,
    // Bulk membership is bounded by the loaded history, which can grow when
    // a shallow repository is deepened. Parent links can change as well.
    member_history: Vec<(String, Vec<String>)>,
    hidden: HashSet<usize>,
    graphs: HashMap<usize, Vec<String>>,
    counts: HashMap<String, usize>,
    ancestry: HashMap<String, Vec<String>>,
    ancestry_ready: bool,
    // Positions of the loaded commits, valid until the next refresh.
    index: HashMap<String, usize>,
    indexed: bool,
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
        } else if self.foldable.contains(&commit.hash) {
            Some("▼")
        } else {
            None
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
        let history: Vec<_> = commits
            .iter()
            .filter(|c| c.kind == CommitKind::Revision)
            .map(|c| (c.hash.clone(), c.parents.clone()))
            .collect();
        // Topological order lists ancestors below descendants, so rows added
        // above an unchanged history cannot be merged by its existing merges.
        if !history.ends_with(&self.member_history) {
            self.members.clear();
        }
        self.member_history = history;
        self.indexed = false;
        self.sync(commits);
        let hashes: HashSet<_> = commits.iter().map(|c| c.hash.clone()).collect();
        self.collapsed.retain(|hash| hashes.contains(hash));
        self.members.retain(|hash, _| hashes.contains(hash));
        self.seen.retain(|hash| hashes.contains(hash));
        let result = (|| {
            // Recompute existing folds after invalidation without changing
            // which merges the user explicitly expanded or collapsed, and
            // resolve the new merges that start collapsed.
            let starting: HashSet<_> = commits
                .iter()
                .filter(|c| self.start_collapsed && !self.seen.contains(&c.hash))
                .filter(|c| self.offers_fold(c, commits))
                .map(|c| c.hash.clone())
                .collect();
            let merges: Vec<_> = commits
                .iter()
                .filter(|c| self.collapsed.contains(&c.hash) || starting.contains(&c.hash))
                .collect();
            self.load_all_members(&merges, commits)?;
            for hash in starting {
                if self.members[&hash].iter().any(|h| hashes.contains(h)) {
                    self.collapsed.insert(hash);
                }
            }
            self.seen.extend(hashes.iter().cloned());
            Ok(())
        })();
        self.foldable = commits
            .iter()
            .filter(|c| self.offers_fold(c, commits))
            .map(|c| c.hash.clone())
            .collect();
        // Even on an I/O error, indices must refer to the new commit list.
        let graph = self.prepare_graph(commits);
        self.graph_ready = result.is_ok() && graph.is_ok();
        self.rebuild(commits);
        result.and(graph)
    }

    /// Known members decide exactly. Otherwise only a loaded side parent
    /// offers a fold: resolving merges whose side parent a filter omitted
    /// would walk the history the filter left unloaded.
    fn offers_fold(&self, merge: &Commit, commits: &[Commit]) -> bool {
        if merge.parents.len() < 2 {
            return false;
        }
        match self.members.get(&merge.hash) {
            Some(members) => members.iter().any(|h| self.index.contains_key(h)),
            None => self.has_side_parent_row(merge, commits),
        }
    }

    /// Whether a loaded side parent is not reachable from the first parent
    /// through loaded rows. Parent links lead down the topological order, so
    /// a path to the side parent only passes rows above it.
    fn has_side_parent_row(&self, merge: &Commit, commits: &[Commit]) -> bool {
        merge.parents[1..]
            .iter()
            .filter_map(|parent| self.index.get(parent).copied())
            .any(|side| {
                let mut todo: Vec<_> = merge.parents[..1].iter().collect();
                let mut visited = HashSet::new();
                while let Some(hash) = todo.pop() {
                    match self.index.get(hash).copied() {
                        Some(index) if index == side => return false,
                        Some(index) if index < side && visited.insert(index) => {
                            todo.extend(&commits[index].parents)
                        }
                        _ => {}
                    }
                }
                true
            })
    }

    fn load_members(&mut self, commit: &Commit) -> Result<(), String> {
        if !self.members.contains_key(&commit.hash) {
            self.members
                .insert(commit.hash.clone(), git::merged_commits(&commit.hash)?);
        }
        Ok(())
    }

    fn load_all_members(&mut self, merges: &[&Commit], commits: &[Commit]) -> Result<(), String> {
        let missing: Vec<_> = merges
            .iter()
            .filter(|c| !self.members.contains_key(&c.hash))
            .collect();
        if let [commit] = missing.as_slice() {
            self.load_members(commit)?;
        } else if !missing.is_empty() {
            let hashes: Vec<_> = missing.iter().map(|c| c.hash.as_str()).collect();
            self.members
                .extend(git::merged_commits_many(&hashes, commits)?);
        }
        Ok(())
    }

    pub fn toggle(&mut self, index: usize, commits: &[Commit]) -> Result<(), String> {
        let Some(commit) = commits.get(index).filter(|c| c.parents.len() > 1) else {
            return Ok(());
        };
        self.discard_unshown_folds();
        let previous = self.collapsed.clone();
        if !self.collapsed.remove(&commit.hash) {
            self.load_members(commit)?;
            if !commits
                .iter()
                .any(|c| self.members[&commit.hash].contains(&c.hash))
            {
                self.foldable.remove(&commit.hash);
                return Err(
                    "No merged commits in this history; revision and path filters still apply"
                        .into(),
                );
            }
            self.collapsed.insert(commit.hash.clone());
            self.foldable.insert(commit.hash.clone());
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
        self.discard_unshown_folds();
        self.sync(commits);
        let candidates: Vec<_> = commits
            .iter()
            .filter(|c| self.offers_fold(c, commits))
            .collect();
        self.load_all_members(&candidates, commits)?;
        let eligible: HashSet<_> = candidates
            .iter()
            .filter(|c| {
                self.members[&c.hash]
                    .iter()
                    .any(|hash| self.index.contains_key(hash))
            })
            .map(|c| c.hash.clone())
            .collect();
        if eligible.is_empty() {
            return Err(
                "No foldable merges in this history; revision and path filters still apply".into(),
            );
        }
        self.foldable.extend(eligible.iter().cloned());
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

    // A failed refresh keeps fold choices for the next successful one, but
    // shows the Log unfolded. Toggles act on what is shown instead.
    fn discard_unshown_folds(&mut self) {
        if !self.graph_ready {
            self.collapsed.clear();
        }
    }

    // Index the commit list once per refresh rather than on every keypress.
    fn sync(&mut self, commits: &[Commit]) {
        if !self.indexed {
            self.index = commits
                .iter()
                .enumerate()
                .map(|(index, c)| (c.hash.clone(), index))
                .collect();
            self.indexed = true;
            self.ancestry_ready = false;
        }
    }

    fn prepare_graph(&mut self, commits: &[Commit]) -> Result<(), String> {
        self.sync(commits);
        if self.collapsed.is_empty() || self.ancestry_ready {
            return Ok(());
        }
        let revisions: Vec<_> = commits
            .iter()
            .filter(|c| c.kind == CommitKind::Revision)
            .collect();
        let boundary: HashSet<_> = revisions
            .last()
            .into_iter()
            .flat_map(|c| &c.parents)
            .collect();
        let missing = revisions
            .iter()
            .flat_map(|c| &c.parents)
            .any(|p| !self.index.contains_key(p) && !boundary.contains(p));
        self.ancestry = if missing {
            git::log_ancestry(commits)?
        } else {
            HashMap::new()
        };
        self.ancestry_ready = true;
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
        self.sync(commits);
        self.hidden.clear();
        self.graphs.clear();
        if !self.graph_ready {
            return;
        }
        self.counts = self
            .collapsed
            .iter()
            .map(|hash| {
                let count = self.members.get(hash).map_or(0, |members| {
                    members
                        .iter()
                        .filter(|member| self.index.contains_key(*member))
                        .count()
                });
                (hash.clone(), count)
            })
            .collect();
        // Only shown folds hide commits: a fold inside another may have merged
        // the outer merge's mainline. Members lie below their merge, so each
        // fold is known to be hidden or shown before it is reached.
        for (index, commit) in commits.iter().enumerate() {
            if self.hidden.contains(&index) || !self.collapsed.contains(&commit.hash) {
                continue;
            }
            if let Some(members) = self.members.get(&commit.hash) {
                self.hidden.extend(
                    members
                        .iter()
                        .filter_map(|hash| self.index.get(hash).copied()),
                );
            }
        }
        if self.hidden.is_empty() {
            return;
        }
        // Each lane keeps a colour from Git's default palette while it lasts.
        let mut lanes = Vec::<(String, usize)>::new();
        let mut colours = 0..;
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
                .position(|(hash, _)| hash == &commit.hash)
                .unwrap_or_else(|| {
                    lanes.push((commit.hash.clone(), colours.next().unwrap()));
                    lanes.len() - 1
                });
            let node: String = lanes
                .iter()
                .enumerate()
                .map(|(i, &(_, colour))| {
                    if i == column {
                        "* ".to_owned()
                    } else {
                        format!("{} ", paint('|', colour))
                    }
                })
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
                let Some(&parent_index) = self.index.get(parent) else {
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
            let (_, colour) = next.remove(column);
            let mut insert = column;
            for parent in &projected {
                if !next.iter().any(|(hash, _)| hash == parent) {
                    // The first new parent continues this lane's colour.
                    let colour = if insert == column {
                        colour
                    } else {
                        colours.next().unwrap()
                    };
                    next.insert(insert, (parent.clone(), colour));
                    insert += 1;
                }
            }
            let mut edges = Vec::new();
            for (from, (hash, _)) in lanes.iter().enumerate() {
                let targets = if from == column {
                    &projected[..]
                } else {
                    std::slice::from_ref(hash)
                };
                for target in targets {
                    if let Some(to) = next.iter().position(|(h, _)| h == target) {
                        edges.push((2 * from, 2 * to, next[to].1));
                    }
                }
            }
            pending = transitions(&edges, lanes.len().max(next.len()));
            lanes = next;
        }
    }
}

// Git's default graph colours, cycled per lane.
const PALETTE: [&str; 12] = [
    "31", "32", "33", "34", "35", "36", "1;31", "1;32", "1;33", "1;34", "1;35", "1;36",
];

fn paint(symbol: char, colour: usize) -> String {
    format!("\x1b[{}m{symbol}\x1b[m", PALETTE[colour % PALETTE.len()])
}

// Route edges a character at a time, each in its target lane's colour.
// Crossings retain their independent targets; '+' joins routes to the same
// target, 'X' crosses routes to different targets.
fn transitions(edges: &[(usize, usize, usize)], width: usize) -> Vec<String> {
    let distance = edges
        .iter()
        .map(|(a, b, _)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    let mut rows = Vec::new();
    for step in 1..=distance {
        let mut cells = vec![(' ', 0); width * 2];
        let mut targets = vec![None; width * 2];
        for &(from, to, colour) in edges {
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
            let symbol = if cells[position].0 == ' ' {
                symbol
            } else if targets[position] == Some(to) {
                '+'
            } else {
                'X'
            };
            cells[position] = (symbol, colour);
            targets[position] = Some(to);
        }
        let used = cells
            .iter()
            .rposition(|&(c, _)| c != ' ')
            .map_or(0, |i| i + 1);
        let row: String = cells[..used]
            .iter()
            .map(|&(symbol, colour)| {
                if symbol == ' ' {
                    " ".to_owned()
                } else {
                    paint(symbol, colour)
                }
            })
            .collect();
        rows.push(format!("{row} "));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                crate::ansi::parse_line(row)
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

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
        assert_eq!(plain(folds.graph(0, &commits[0])), ["* "]);
        assert_eq!(plain(folds.graph(1, &commits[1])), ["| * "]);
        assert_eq!(plain(folds.graph(3, &commits[3])), ["* | "]);
        assert_eq!(plain(folds.graph(4, &commits[4])), ["|/ ", "+ ", "* "]);
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
    fn loaded_side_history_shows_a_disclosure_marker() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(folds.marker(&commits[0]), Some("▼"));
    }

    #[test]
    fn folded_graph_colours_lanes_like_git() {
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
        let row = &folds.graph(1, &commits[1])[0];
        assert!(row.contains("\x1b[3"), "{row:?}");
    }

    #[test]
    fn folding_all_without_foldable_merges_explains_why() {
        let commits = vec![commit("one", &["base"]), commit("base", &[])];
        assert!(LogFolds::default().toggle_all(0, &commits).is_err());
    }

    #[test]
    fn first_parent_history_resolves_merges_without_an_ancestry_walk() {
        // As in a --first-parent log, side parents are never loaded and each
        // row below a merge is its mainline history. These hashes are in no
        // repository, so a Git walk would fail the refresh.
        let commits = vec![
            commit("two", &["one", "side two"]),
            commit("one", &["base", "side one"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds {
            start_collapsed: true,
            ..LogFolds::default()
        };
        folds.refresh(&commits).unwrap();
        assert!(commits.iter().all(|c| folds.marker(c).is_none()));
        let error = folds.toggle_all(0, &commits).unwrap_err();
        assert!(error.contains("No foldable merges"), "{error}");
    }

    #[test]
    fn new_commits_on_top_keep_existing_merge_members() {
        let commits = vec![
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        folds
            .members
            .insert("merge".into(), HashSet::from(["side".into()]));
        folds.toggle(0, &commits).unwrap();
        // These hashes are in no repository, so reloading members would fail.
        let mut refreshed = vec![commit("new", &["merge"])];
        refreshed.extend(commits);
        folds.refresh(&refreshed).unwrap();
        assert!(!folds.visible(2));
        assert_eq!(folds.marker(&refreshed[1]), Some("▶"));
    }

    #[test]
    fn side_parent_in_mainline_history_shows_no_disclosure_marker() {
        // As with rewritten parents under --full-history -- path, the loaded
        // side parent is also an ancestor of the first parent. An unrelated
        // row keeps the history from being a single first-parent chain.
        let commits = vec![
            commit("merge", &["main", "old"]),
            commit("main", &["old"]),
            commit("unrelated", &["base"]),
            commit("old", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        assert_eq!(folds.marker(&commits[0]), None);
    }

    #[test]
    fn folding_after_a_failed_refresh_folds_on_the_first_press() {
        let commits = vec![
            // Starting collapsed resolves this merge, which fails because
            // these hashes are in no repository.
            commit("unresolved", &["merge", "other side"]),
            commit("other side", &["base"]),
            commit("merge", &["main", "side"]),
            commit("side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds
            .members
            .insert("merge".into(), HashSet::from(["side".into()]));
        folds.toggle(2, &commits).unwrap();
        folds.start_collapsed = true;
        assert!(folds.refresh(&commits).is_err());
        // The failure leaves the Log unfolded, so z must fold what is shown.
        assert!(folds.visible(3));
        assert_eq!(folds.marker(&commits[2]), Some("▼"));
        folds.toggle(2, &commits).unwrap();
        assert!(!folds.visible(3));
        assert_eq!(folds.marker(&commits[2]), Some("▶"));
    }

    #[test]
    fn filtered_out_side_parents_are_not_resolved_by_bulk_folding() {
        // As in a --grep log, the side tip is omitted but an older side commit
        // matched. These hashes are in no repository, so a walk would fail.
        let commits = vec![
            commit("merge", &["main", "omitted tip"]),
            commit("older side", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds {
            start_collapsed: true,
            ..LogFolds::default()
        };
        folds.refresh(&commits).unwrap();
        assert_eq!(folds.marker(&commits[0]), None);
        assert!(folds.visible(1));
        let error = folds.toggle_all(0, &commits).unwrap_err();
        assert!(error.contains("No foldable merges"), "{error}");
    }

    #[test]
    fn nested_folds_keep_mainline_that_a_topic_merged_back() {
        // The topic merged master's m1 before master merged the topic, so
        // m1 is merged by x but is mainline history of the outer merge.
        let commits = vec![
            commit("outer", &["m2", "t2"]),
            commit("t2", &["x"]),
            commit("x", &["t1", "m1"]),
            commit("t1", &["base"]),
            commit("m2", &["m1"]),
            commit("m1", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.members.insert(
            "outer".into(),
            HashSet::from(["t2".into(), "x".into(), "t1".into()]),
        );
        folds
            .members
            .insert("x".into(), HashSet::from(["m1".into()]));
        folds.toggle_all(0, &commits).unwrap();
        let visible: Vec<_> = (0..commits.len())
            .filter(|&i| folds.visible(i))
            .map(|i| commits[i].hash.as_str())
            .collect();
        assert_eq!(visible, ["outer", "m2", "m1", "base"]);
    }

    #[test]
    fn expanding_an_outer_merge_keeps_mainline_a_folded_topic_merged_back() {
        // As above, but with the outer merge expanded, x's fold is shown.
        // m1 is still reached from m2, so it remains visible.
        let commits = vec![
            commit("outer", &["m2", "t2"]),
            commit("t2", &["x"]),
            commit("x", &["t1", "m1"]),
            commit("t1", &["base"]),
            commit("m2", &["m1"]),
            commit("m1", &["base"]),
            commit("base", &[]),
        ];
        let mut folds = LogFolds::default();
        folds.members.insert(
            "outer".into(),
            HashSet::from(["t2".into(), "x".into(), "t1".into()]),
        );
        folds
            .members
            .insert("x".into(), HashSet::from(["m1".into()]));
        folds.toggle_all(0, &commits).unwrap();
        folds.toggle(0, &commits).unwrap();
        assert!((0..commits.len()).all(|i| folds.visible(i)));
    }

    #[test]
    fn crossing_routes_do_not_turn_into_a_shared_parent() {
        assert_eq!(
            plain(&transitions(&[(0, 2, 0), (2, 0, 1)], 2)),
            [" X ", "/ \\ "]
        );
    }
}
