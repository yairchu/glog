//! Subject-based relationships within the loaded, newest-first Log history.
use std::collections::HashMap;

use crate::git::{Commit, CommitKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Commit(usize),
    Ambiguous,
    Missing,
}

pub struct Label {
    pub prefix_len: usize,
    pub text: String,
    pub target: Option<usize>,
}

fn prefix(subject: &str) -> Option<&str> {
    ["fixup! ", "squash! ", "amend! "]
        .into_iter()
        .find(|prefix| subject.starts_with(prefix))
}

/// Only older exact subjects qualify. Duplicate subjects remain ambiguous,
/// even if one happens to be closer to the fixup in the display.
pub fn resolve(commits: &[Commit]) -> Vec<Option<Target>> {
    let mut targets = vec![None; commits.len()];
    // Track only subjects named by fixups: large histories should not allocate
    // an index of every commit on each redraw.
    let mut subjects: HashMap<_, _> = commits
        .iter()
        .filter(|commit| commit.kind == CommitKind::Revision && commit.annotation.is_none())
        .filter_map(|commit| {
            prefix(&commit.subject).map(|prefix| (&commit.subject[prefix.len()..], Target::Missing))
        })
        .collect();
    if subjects.is_empty() {
        return targets;
    }
    for (index, commit) in commits.iter().enumerate().rev() {
        if commit.kind != CommitKind::Revision || commit.annotation.is_some() {
            continue;
        }
        if let Some(prefix) = prefix(&commit.subject) {
            targets[index] = Some(
                subjects
                    .get(&commit.subject[prefix.len()..])
                    .copied()
                    .unwrap_or(Target::Missing),
            );
        }
        if let Some(target) = subjects.get_mut(commit.subject.as_str()) {
            *target = match target {
                Target::Missing => Target::Commit(index),
                Target::Commit(_) | Target::Ambiguous => Target::Ambiguous,
            };
        }
    }
    targets
}

impl Target {
    pub fn label(self, commit: &Commit, commits: &[Commit]) -> Label {
        let (text, target) = match self {
            Self::Commit(index) => (commits[index].short_hash.clone(), Some(index)),
            Self::Ambiguous => ("ambiguous".into(), None),
            Self::Missing => ("missing".into(), None),
        };
        Label {
            prefix_len: prefix(&commit.subject).unwrap().len(),
            text,
            target,
        }
    }
}
