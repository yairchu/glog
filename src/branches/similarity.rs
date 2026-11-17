//! Patch equivalence across the pinned tips in the branch picker.
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::{
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
};

use crate::{
    compact_graph::Graph,
    git::{self, Annotation, Commit, Repository},
};

#[derive(Debug, PartialEq, Eq)]
struct Tip {
    hash: String,
    branches: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Source {
    graph: Arc<Graph>,
    tips: Vec<Tip>,
}

impl Source {
    pub(super) fn new(
        graph: Arc<Graph>,
        tips: &HashMap<String, Vec<super::Branch>>,
    ) -> Option<Arc<Self>> {
        let mut tips: Vec<_> = tips
            .iter()
            .filter(|(_, branches)| !branches.is_empty())
            .map(|(hash, branches)| Tip {
                hash: hash.clone(),
                branches: branches.len(),
            })
            .collect();
        tips.sort_by(|a, b| a.hash.cmp(&b.hash));
        (tips.len() > 1).then(|| Arc::new(Self { graph, tips }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Summary {
    patches: usize,
    branches: usize,
}

impl Summary {
    pub(super) fn label(&self) -> String {
        format!(
            "{} equivalent patch{} across {} other branch{}",
            self.patches,
            if self.patches == 1 { "" } else { "es" },
            self.branches,
            if self.branches == 1 { "" } else { "es" }
        )
    }
}

type Cache = HashMap<String, Option<String>>;
type Summaries = HashMap<String, Summary>;

struct Loaded {
    summaries: Summaries,
    cache: Arc<Cache>,
}

struct Pending {
    receiver: Receiver<Option<Loaded>>,
    cancel: [git::Cancel; 2],
}

impl Drop for Pending {
    fn drop(&mut self) {
        for cancel in &self.cancel {
            cancel.cancel();
        }
    }
}

#[derive(Default)]
pub(crate) struct State {
    source: Option<Arc<Source>>,
    pending: Option<Pending>,
    summaries: Summaries,
    cache: Arc<Cache>,
}

impl State {
    pub(crate) fn loading(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn poll(&mut self, rows: &mut [Commit]) -> bool {
        let source = rows
            .iter()
            .find_map(|row| row.branch()?.similarity_source.clone());
        let same = match (&self.source, &source) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new) || old == new,
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.pending = None;
            self.summaries.clear();
            if let (Some(source), Ok(repository)) = (&source, Repository::main()) {
                let source = source.clone();
                let cache = self.cache.clone();
                let (sender, receiver) = mpsc::channel();
                let cancel = [git::Cancel::default(), git::Cancel::default()];
                let worker_cancel = cancel.clone();
                std::thread::spawn(move || {
                    let _ = sender.send(compare(&source, &repository, cache, &worker_cancel));
                });
                self.pending = Some(Pending { receiver, cancel });
            }
        }
        // Remember the new Arc even for identical watch reloads, so ordinary
        // UI polls compare pointers rather than walking the full graph.
        self.source = source;
        if let Some(pending) = &self.pending {
            match pending.receiver.try_recv() {
                Ok(result) => {
                    if let Some(loaded) = result {
                        self.summaries = loaded.summaries;
                        self.cache = loaded.cache;
                    }
                    self.pending = None;
                }
                Err(TryRecvError::Disconnected) => self.pending = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let mut changed = false;
        for row in rows {
            if let Some(Annotation::Branch(node)) = &mut row.annotation {
                let summary = self.summaries.get(&row.hash).cloned();
                changed |= node.similarity != summary;
                node.similarity = summary;
            }
        }
        changed
    }
}

fn compare(
    source: &Source,
    repository: &Repository,
    mut cache: Arc<Cache>,
    cancel: &[git::Cancel; 2],
) -> Option<Loaded> {
    let graph = &source.graph;
    let mut owners = vec![BTreeSet::new(); graph.nodes.len()];
    for (tip, branch) in source.tips.iter().enumerate() {
        owners[*graph.indices.get(&branch.hash)?].insert(tip);
    }
    for (index, node) in graph.nodes.iter().enumerate() {
        if index % 128 == 0 && cancel[0].is_cancelled() {
            return None;
        }
        for &parent in &node.parents {
            let (children, parents) = owners.split_at_mut(parent);
            parents[0].extend(children[index].iter().copied());
        }
    }
    // Common ancestry cannot supply a match absent from another branch, and
    // commits only remote counterparts reach supply none to any branch.
    // Merge patches and empty commits do not have a single comparable change.
    let candidates: Vec<_> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(index, node)| {
            node.parents.len() < 2
                && !owners[*index].is_empty()
                && owners[*index].len() < source.tips.len()
        })
        .map(|(index, _)| index)
        .collect();
    let missing: Vec<_> = candidates
        .iter()
        .map(|&index| &graph.nodes[index].hash)
        .filter(|hash| !cache.contains_key(*hash))
        .cloned()
        .collect();
    let ids = git::patch_ids(repository, &missing, cancel)?;
    if !missing.is_empty() {
        let cache = Arc::make_mut(&mut cache);
        for hash in missing {
            cache.insert(hash.clone(), ids.get(&hash).cloned());
        }
    }
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for index in candidates {
        if let Some(Some(id)) = cache.get(&graph.nodes[index].hash) {
            groups.entry(id).or_default().push(index);
        }
    }
    let mut patches = vec![0; source.tips.len()];
    let mut peers = vec![HashSet::new(); source.tips.len()];
    for commits in groups.values().filter(|commits| commits.len() > 1) {
        if cancel[0].is_cancelled() {
            return None;
        }
        let mut matched = HashSet::new();
        let mut patterns: Vec<_> = commits.iter().map(|&index| &owners[index]).collect();
        patterns.sort_unstable();
        patterns.dedup();
        for (index, left) in patterns.iter().enumerate() {
            for right in &patterns[index + 1..] {
                // Both sides must have a distinct commit absent from the
                // other side. An inherited commit is not a patch match.
                let left_only: Vec<_> = left.difference(right).copied().collect();
                let right_only: Vec<_> = right.difference(left).copied().collect();
                if left_only.is_empty() || right_only.is_empty() {
                    continue;
                }
                for &owner in &left_only {
                    matched.insert(owner);
                    peers[owner].extend(right_only.iter().copied());
                }
                for &owner in &right_only {
                    matched.insert(owner);
                    peers[owner].extend(left_only.iter().copied());
                }
            }
        }
        for owner in matched {
            patches[owner] += 1;
        }
    }
    let summaries = source
        .tips
        .iter()
        .enumerate()
        .filter(|(index, _)| patches[*index] > 0)
        .map(|(index, tip)| {
            (
                tip.hash.clone(),
                Summary {
                    patches: patches[index],
                    branches: peers[index]
                        .iter()
                        .map(|&peer| source.tips[peer].branches)
                        .sum(),
                },
            )
        })
        .collect();
    Some(Loaded { summaries, cache })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};
    use std::{
        fs,
        path::PathBuf,
        time::{Duration, Instant},
    };

    fn commit(path: &str, contents: &str, message: &str) -> String {
        fs::write(path, contents).unwrap();
        git(&["add", "--", path]);
        git(&["commit", "-qm", message]);
        git(&["rev-parse", "HEAD"])
    }

    fn settle(app: &mut crate::app::App) {
        let deadline = Instant::now() + Duration::from_secs(10);
        app.branch_similarity.poll(&mut app.commits);
        while app.branch_similarity.loading() {
            assert!(Instant::now() < deadline, "patch comparison stalled");
            std::thread::sleep(Duration::from_millis(10));
            app.branch_similarity.poll(&mut app.commits);
        }
    }

    fn summary<'a>(app: &'a crate::app::App, name: &str) -> Option<&'a Summary> {
        app.commits.iter().find_map(|row| {
            let node = row.branch()?;
            node.branches
                .iter()
                .any(|branch| branch.name == name)
                .then_some(node.similarity.as_ref())
                .flatten()
        })
    }

    #[test]
    fn rebased_patches_are_counted_once_and_cached_across_watch_refreshes() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        commit("base.txt", "base\n", "base");
        git(&["checkout", "-qb", "original"]);
        let first = commit("space 界.txt", "feature\n", "first feature");
        let second = commit("second.txt", "second\n", "second feature");
        git(&["checkout", "-qb", "stacked"]);
        commit("stacked.txt", "stacked\n", "stacked change");
        git(&["checkout", "-q", "main"]);
        commit("unrelated.txt", "advance\n", "advance main");
        git(&["checkout", "-qb", "rebased"]);
        git(&["cherry-pick", &first, &second]);
        git(&[
            "commit",
            "--amend",
            "-qm",
            "a different message for the rebased change",
        ]);
        git(&["branch", "copy"]);
        // Display settings and user diff programs must not change fingerprints.
        git(&["config", "diff.noprefix", "true"]);
        git(&["config", "diff.context", "0"]);
        git(&["config", "diff.external", "a-nonexistent-diff-program"]);
        let mut app = super::super::load_picker().unwrap();
        settle(&mut app);
        assert_eq!(
            summary(&app, "original"),
            Some(&Summary {
                patches: 2,
                branches: 2
            })
        );
        assert_eq!(
            summary(&app, "stacked"),
            Some(&Summary {
                patches: 2,
                branches: 2
            })
        );
        assert_eq!(
            summary(&app, "rebased"),
            Some(&Summary {
                patches: 2,
                branches: 2
            })
        );
        assert!(summary(&app, "main").is_none());
        let rendered: String = super::super::summary(&app)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(rendered.contains("2 equivalent patches across 2 other branches"));
        let cache = app.branch_similarity.cache.clone();
        app.commits = super::super::load_watched(&[]).unwrap();
        assert!(app.branch_similarity.poll(&mut app.commits));
        assert!(
            !app.branch_similarity.loading(),
            "an unchanged graph should reuse summaries"
        );
        assert!(Arc::ptr_eq(&cache, &app.branch_similarity.cache));
        // A changed tip set recalculates membership without rereading patches.
        git(&["branch", "third-copy", "rebased"]);
        app.commits = super::super::load_watched(&[]).unwrap();
        settle(&mut app);
        assert_eq!(
            summary(&app, "original"),
            Some(&Summary {
                patches: 2,
                branches: 3
            })
        );
        assert!(Arc::ptr_eq(&cache, &app.branch_similarity.cache));
        git(&["checkout", "-q", "original"]);
        git(&["branch", "-D", "rebased", "copy", "third-copy"]);
        app.commits = super::super::load_watched(&[]).unwrap();
        settle(&mut app);
        assert!(
            app.branch_similarity.summaries.is_empty(),
            "inherited commits must not count as equivalent patches"
        );
    }

    #[test]
    fn shared_commits_and_repeated_patches_on_one_history_do_not_create_matches() {
        // a reaches both versions; b and c each have only one. Only b and c
        // are equivalent across distinct histories. a shares theirs by ancestry.
        let source = Source {
            graph: Arc::new(Graph::parse("a b c\nb r\nc r\nr\n").unwrap()),
            tips: ["a", "b", "c"]
                .into_iter()
                .map(|hash| Tip {
                    hash: hash.into(),
                    branches: 1,
                })
                .collect(),
        };
        let cache: Cache = [
            ("b".into(), Some("same".into())),
            ("c".into(), Some("same".into())),
        ]
        .into();
        let loaded = compare(
            &source,
            &Repository::checked_out(PathBuf::new()),
            Arc::new(cache),
            &[git::Cancel::default(), git::Cancel::default()],
        )
        .unwrap();
        assert_eq!(loaded.summaries.len(), 2);
        assert_eq!(
            loaded.summaries["b"],
            Summary {
                patches: 1,
                branches: 1
            }
        );
        assert_eq!(
            loaded.summaries["c"],
            Summary {
                patches: 1,
                branches: 1
            }
        );
        assert!(!loaded.summaries.contains_key("a"));
    }

    #[test]
    fn commits_no_branch_reaches_are_not_fingerprinted() {
        // Remote counterparts add x to the ancestry without owning it.
        let source = Source {
            graph: Arc::new(Graph::parse("a r\nb r\nx r\nr\n").unwrap()),
            tips: ["a", "b"]
                .into_iter()
                .map(|hash| Tip {
                    hash: hash.into(),
                    branches: 1,
                })
                .collect(),
        };
        let cache: Cache = [("a".into(), None), ("b".into(), None)].into();
        // The repository does not exist, so any patch-id read fails.
        let loaded = compare(
            &source,
            &Repository::checked_out(PathBuf::new()),
            Arc::new(cache),
            &[git::Cancel::default(), git::Cancel::default()],
        )
        .expect("only commits branches reach need fingerprints");
        assert!(!loaded.cache.contains_key("x"));
    }

    #[test]
    fn binary_patches_match_but_empty_commits_and_equal_messages_do_not() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        commit("base.txt", "base\n", "base");
        git(&["checkout", "-qb", "original"]);
        fs::write("image.bin", [0, 1, 2, 3]).unwrap();
        git(&["add", "image.bin"]);
        git(&["commit", "-qm", "same message"]);
        let binary = git(&["rev-parse", "HEAD"]);
        git(&["commit", "--allow-empty", "-qm", "empty"]);
        git(&["checkout", "-q", "main"]);
        commit("advance.txt", "advance\n", "advance");
        git(&["checkout", "-qb", "rebased"]);
        git(&["cherry-pick", &binary]);
        git(&["commit", "--allow-empty", "-qm", "another empty"]);
        git(&["checkout", "-qb", "different", "main"]);
        fs::write("image.bin", [0, 4, 5, 6]).unwrap();
        git(&["add", "image.bin"]);
        git(&["commit", "-qm", "same message"]);
        let mut app = super::super::load_picker().unwrap();
        settle(&mut app);
        assert_eq!(
            summary(&app, "original"),
            Some(&Summary {
                patches: 1,
                branches: 1
            })
        );
        assert_eq!(
            summary(&app, "rebased"),
            Some(&Summary {
                patches: 1,
                branches: 1
            })
        );
        assert!(summary(&app, "different").is_none());
    }

    #[test]
    fn a_replaced_snapshot_cancels_and_discards_its_pending_result() {
        let source = |tip: &str| {
            Arc::new(Source {
                graph: Arc::new(Graph::parse(&format!("{tip} r\nb r\nr\n")).unwrap()),
                tips: [tip, "b"]
                    .into_iter()
                    .map(|hash| Tip {
                        hash: hash.into(),
                        branches: 1,
                    })
                    .collect(),
            })
        };
        let old = source("a");
        let (sender, receiver) = mpsc::channel();
        let cancel = [git::Cancel::default(), git::Cancel::default()];
        let mut state = State {
            source: Some(old),
            pending: Some(Pending {
                receiver,
                cancel: cancel.clone(),
            }),
            ..Default::default()
        };
        // Removing all rows invalidates the request even if its worker has a
        // result ready. It must never annotate a later branch snapshot.
        sender
            .send(Some(Loaded {
                summaries: [(
                    "a".into(),
                    Summary {
                        patches: 9,
                        branches: 9,
                    },
                )]
                .into(),
                cache: Arc::default(),
            }))
            .unwrap();
        state.poll(&mut []);
        assert!(!state.loading());
        assert!(state.summaries.is_empty());
        assert!(cancel.iter().all(git::Cancel::is_cancelled));
    }
}
