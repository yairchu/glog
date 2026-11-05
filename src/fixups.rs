//! Subject-based relationships within the loaded Log history.
use std::collections::{HashMap, HashSet};

use crate::git::{Commit, CommitKind, Repository};

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

/// What a fixup names, after the repeated prefixes autosquash also skips.
fn named(subject: &str) -> Option<&str> {
    let mut rest = &subject[prefix(subject)?.len()..];
    loop {
        rest = rest.trim_start();
        match prefix(rest) {
            Some(prefix) => rest = &rest[prefix.len()..],
            None => return Some(rest),
        }
    }
}

fn eligible(commit: &Commit) -> bool {
    commit.kind == CommitKind::Revision && commit.annotation.is_none()
}

/// Match the way `git rebase --autosquash` does, but against older commits
/// only: an exact subject, else a revision name without spaces, else a
/// subject prefix. Where autosquash would pick one of several equal
/// candidates, the target remains ambiguous, even if one happens to be closer
/// to the fixup in the display.
pub fn resolve(commits: &[Commit], repository: Option<&Repository>) -> Vec<Option<Target>> {
    let mut targets = vec![None; commits.len()];
    let named_by_fixups: HashSet<_> = commits
        .iter()
        .filter(|commit| eligible(commit))
        .filter_map(|commit| named(&commit.subject))
        .collect();
    if named_by_fixups.is_empty() {
        return targets;
    }
    let revisions = crate::git::resolve_names(
        named_by_fixups
            .iter()
            .copied()
            .filter(|name| !name.is_empty() && !name.contains(' ')),
        repository,
    );
    let named_by_revisions: HashSet<_> = revisions.values().map(String::as_str).collect();
    let reversed = commits.iter().any(|commit| commit.traversal.reversed);
    // Resolve oldest first, retaining display indices for links and selection.
    let older: Vec<_> = (0..commits.len())
        .map(|position| {
            if reversed {
                position
            } else {
                commits.len() - 1 - position
            }
        })
        .filter(|&index| eligible(&commits[index]))
        .collect();
    // Track only subjects and hashes that fixups name, not every commit.
    let mut subjects: HashMap<&str, Target> = HashMap::new();
    let mut hashes: HashMap<&str, usize> = HashMap::new();
    for (position, &index) in older.iter().enumerate() {
        let commit = &commits[index];
        if let Some(name) = named(&commit.subject) {
            let target = subjects.get(name).copied().or_else(|| {
                let hash = revisions.get(name)?;
                hashes.get(hash.as_str()).copied().map(Target::Commit)
            });
            targets[index] = Some(target.unwrap_or_else(|| {
                unique(
                    older[..position]
                        .iter()
                        .copied()
                        .filter(|&older| commits[older].subject.starts_with(name)),
                )
            }));
        }
        if named_by_fixups.contains(commit.subject.as_str()) {
            subjects
                .entry(&commit.subject)
                .and_modify(|target| *target = Target::Ambiguous)
                .or_insert(Target::Commit(index));
        }
        if named_by_revisions.contains(commit.hash.as_str()) {
            hashes.insert(&commit.hash, index);
        }
    }
    targets
}

fn unique(mut candidates: impl Iterator<Item = usize>) -> Target {
    match (candidates.next(), candidates.next()) {
        (None, _) => Target::Missing,
        (Some(index), None) => Target::Commit(index),
        (Some(_), Some(_)) => Target::Ambiguous,
    }
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
    fn fixups_name_targets_by_hash_and_subject_prefix_like_autosquash() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let commit = |subject: &str| git(&["commit", "--allow-empty", "-qm", subject]);
        commit("Add parser for X");
        commit("Add printer");
        commit("Add printer tests");
        let short = git(&["rev-parse", "--short=7", "HEAD~2"]);
        git(&["branch", "printer-branch", "HEAD~1"]);
        git(&["tag", "-am", "release", "printer-tag", "HEAD~1"]);
        for subject in [
            "fixup! Add parser",
            &format!("fixup! {short}"),
            &format!("squash! {}", short.to_uppercase()),
            "fixup! fixup! Add parser for X",
            "fixup! Add printer",
            "fixup! Add print",
            "fixup! printer-branch",
            "fixup! printer-tag",
            "fixup! deadbeef",
            // Git exits on this name rather than answering it.
            "fixup! HEAD@{999}",
        ] {
            commit(subject);
        }
        let commits = crate::git::load_log(&["--no-graph".into()]).unwrap();
        let targets = resolve(&commits, None);
        let index = |subject: &str| {
            commits
                .iter()
                .position(|commit| commit.subject == subject)
                .unwrap()
        };
        let parser = Some(Target::Commit(index("Add parser for X")));
        for (subject, expected) in [
            ("fixup! Add parser", parser),
            (&format!("fixup! {short}"), parser),
            (&format!("squash! {}", short.to_uppercase()), parser),
            ("fixup! fixup! Add parser for X", parser),
            // An exact subject wins over longer subjects it prefixes.
            (
                "fixup! Add printer",
                Some(Target::Commit(index("Add printer"))),
            ),
            ("fixup! Add print", Some(Target::Ambiguous)),
            (
                "fixup! printer-branch",
                Some(Target::Commit(index("Add printer"))),
            ),
            // Autosquash peels an annotated tag to the commit it tags.
            (
                "fixup! printer-tag",
                Some(Target::Commit(index("Add printer"))),
            ),
            ("fixup! deadbeef", Some(Target::Missing)),
            ("fixup! HEAD@{999}", Some(Target::Missing)),
        ] {
            assert_eq!(targets[index(subject)], expected, "{subject}");
        }
    }

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
            let targets = resolve(&commits, None);
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
