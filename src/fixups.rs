//! Subject-based relationships within the loaded Log history.
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
    // Track only subjects named by fixups, not an index of every commit.
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
    let reversed = commits.iter().any(|commit| commit.log_reversed);
    // Resolve oldest first, retaining display indices for links and selection.
    for position in 0..commits.len() {
        let index = if reversed {
            position
        } else {
            commits.len() - 1 - position
        };
        let commit = &commits[index];
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};

    #[test]
    fn reversed_history_keeps_fixup_targets_and_rejects_newer_or_ambiguous_subjects() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        for subject in [
            "base",
            "fixup! future",
            "future",
            "duplicate",
            "duplicate",
            "fixup! duplicate",
            "fixup! base",
            "squash! base",
            "amend! base",
        ] {
            git(&["commit", "--allow-empty", "-qm", subject]);
        }
        for options in [
            vec![],
            vec!["--reverse"],
            vec!["--reverse", "--reverse"],
            vec!["--grep", "--reverse", "--invert-grep"],
        ] {
            let mut args = vec!["--no-graph".into()];
            args.extend(options.iter().map(|option| (*option).to_owned()));
            let commits = crate::git::load_log(&args).unwrap();
            let targets = resolve(&commits);
            let base = commits
                .iter()
                .position(|commit| commit.subject == "base")
                .unwrap();
            for (index, commit) in commits.iter().enumerate() {
                let expected = match commit.subject.as_str() {
                    "fixup! base" | "squash! base" | "amend! base" => Some(Target::Commit(base)),
                    "fixup! future" => Some(Target::Missing),
                    "fixup! duplicate" => Some(Target::Ambiguous),
                    _ => None,
                };
                assert_eq!(
                    targets[index], expected,
                    "options={options:?}, subject={}",
                    commit.subject
                );
            }
        }
    }
}
