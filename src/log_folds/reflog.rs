//! Reflog folds describe operations, independently of commit ancestry.
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::git::{Annotation, Commit};

use super::{Fold, Layout};

#[derive(Default)]
pub(super) struct Folds {
    groups: BTreeMap<usize, Group>,
}

enum Kind {
    Rebase,
    Amend,
}

struct Group {
    kind: Kind,
    // Newest first, with the entry representing the fold at the head.
    // Entries from other refs may be interleaved in `reflog --all`.
    rows: Vec<usize>,
    expanded: bool,
}

impl Folds {
    pub(super) fn new(commits: &[Commit]) -> Self {
        let mut pending: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut groups = BTreeMap::new();
        for (index, commit) in commits.iter().enumerate() {
            let Some(Annotation::Reflog(entry)) = &commit.annotation else {
                continue;
            };
            let reference = entry.reference.as_str();
            match rebase_event(&entry.action) {
                Some(RebaseEvent::End) => {
                    pending.insert(reference, vec![index]);
                }
                Some(RebaseEvent::Start) => {
                    if let Some(mut rows) = pending.remove(reference) {
                        rows.push(index);
                        insert_group(&mut groups, Kind::Rebase, rows);
                    }
                }
                Some(RebaseEvent::Step) => {
                    if let Some(rows) = pending.get_mut(reference) {
                        rows.push(index);
                    }
                }
                // A checkout or unrecognized action makes the bounds
                // ambiguous. Never hide it as part of a neighboring rebase.
                _ => {
                    pending.remove(reference);
                }
            }
        }
        // Rebase members belong exclusively to their operation, including
        // when its fold is expanded. Never create nested amend folds in it.
        let mut in_rebase = vec![false; commits.len()];
        for group in groups.values() {
            for &row in &group.rows {
                in_rebase[row] = true;
            }
        }
        // Also leave amendments in an unfinished rebase alone.
        // Walk chronologically because such an operation has no end fold.
        let mut active = HashSet::new();
        for (index, commit) in commits.iter().enumerate().rev() {
            let Some(Annotation::Reflog(entry)) = &commit.annotation else {
                continue;
            };
            let reference = entry.reference.as_str();
            match rebase_event(&entry.action) {
                Some(RebaseEvent::Start) => {
                    active.insert(reference);
                }
                Some(RebaseEvent::Step) => {}
                _ => {
                    active.remove(reference);
                }
            }
            in_rebase[index] |= active.contains(reference);
        }
        let mut pending: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, commit) in commits.iter().enumerate() {
            let Some(Annotation::Reflog(entry)) = &commit.annotation else {
                continue;
            };
            let reference = entry.reference.as_str();
            let event = (!in_rebase[index])
                .then(|| commit_event(&entry.action))
                .flatten();
            match event {
                Some(CommitEvent::Amend) => {
                    pending.entry(reference).or_default().push(index);
                }
                _ => {
                    if let Some(mut rows) = pending.remove(reference) {
                        if matches!(event, Some(CommitEvent::Original)) {
                            rows.push(index);
                        }
                        insert_group(&mut groups, Kind::Amend, rows);
                    }
                }
            }
        }
        // A count limit or reflog expiry can omit the original commit.
        for rows in pending.into_values() {
            insert_group(&mut groups, Kind::Amend, rows);
        }
        Self { groups }
    }

    pub(super) fn layout(&self, rows: usize) -> Layout {
        let mut layout = Layout {
            visible: vec![true; rows],
            owners: vec![None; rows],
            folds: vec![Fold::None; rows],
            ..Layout::default()
        };
        for group in self.groups.values() {
            let header = group.rows[0];
            layout.folds[header] = if group.expanded {
                Fold::Foldable
            } else {
                for &row in &group.rows[1..] {
                    layout.visible[row] = false;
                    layout.owners[row] = Some(header);
                }
                Fold::Folded(group.rows.len() - 1)
            };
        }
        layout
    }

    pub(super) fn toggle(&mut self, index: usize) {
        if let Some(group) = self.groups.values_mut().find(|g| g.rows.contains(&index)) {
            group.expanded = !group.expanded;
        }
    }

    pub(super) fn reveal(&mut self, index: usize) {
        if let Some(group) = self
            .groups
            .values_mut()
            .find(|g| g.rows[1..].contains(&index))
        {
            group.expanded = true;
        }
    }

    pub(super) fn label(&self, index: usize) -> String {
        let Some(group) = self.groups.get(&index).filter(|group| !group.expanded) else {
            return String::new();
        };
        let count = group.rows.len() - 1;
        let noun = match group.kind {
            Kind::Rebase => "rebase step",
            Kind::Amend => "earlier version",
        };
        format!(" · {count} {noun}{}", if count == 1 { "" } else { "s" })
    }
}

fn insert_group(groups: &mut BTreeMap<usize, Group>, kind: Kind, rows: Vec<usize>) {
    if rows.len() > 1 {
        groups.insert(
            rows[0],
            Group {
                kind,
                rows,
                expanded: false,
            },
        );
    }
}

enum CommitEvent {
    Amend,
    Original,
}

fn commit_event(action: &str) -> Option<CommitEvent> {
    match action.split_once(": ")?.0 {
        "commit (amend)" => Some(CommitEvent::Amend),
        "commit"
        | "commit (initial)"
        | "commit (cherry-pick)"
        | "commit (merge)"
        | "cherry-pick"
        | "revert" => Some(CommitEvent::Original),
        // "merge <revision>", which can also be a fast-forward.
        action if action.starts_with("merge ") => Some(CommitEvent::Original),
        _ => None,
    }
}

enum RebaseEvent {
    Start,
    End,
    Step,
}

fn rebase_event(action: &str) -> Option<RebaseEvent> {
    // Replaying or editing commits can produce ordinary commit actions.
    // These count as steps only inside a matched start/end sequence.
    if commit_event(action).is_some() || action == "rebase: fast-forward" {
        return Some(RebaseEvent::Step);
    }
    let (action, _) = action.split_once(": ")?;
    // Stages follow the command line, e.g. "rebase -i (pick)" in older Git
    // versions, or "pull -r origin main (pick)". A continued rebase can have
    // more than one parenthesized stage, e.g. (continue) (finish).
    let (command, _) = action.split_once(" (")?;
    if !matches!(command.split(' ').next(), Some("rebase" | "pull")) {
        return None;
    }
    match action.rsplit_once(" (")?.1.strip_suffix(')')? {
        "start" => Some(RebaseEvent::Start),
        "finish" | "abort" => Some(RebaseEvent::End),
        "pick" | "reword" | "edit" | "squash" | "fixup" | "continue" | "reset" | "merge"
        | "skip" | "fast-forward" => Some(RebaseEvent::Step),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{App, LogAction},
        git::ReflogEntry,
    };

    fn app(events: &[(&str, &str)]) -> App {
        App::new(
            events
                .iter()
                .map(|(reference, action)| {
                    // Repeated hashes and timestamps are normal in reflogs;
                    // neither can identify an operation or one of its rows.
                    let mut commit = crate::log_format::tests::commit();
                    commit.graph = vec![String::new()];
                    commit.parents.clear();
                    commit.annotation = Some(Annotation::Reflog(ReflogEntry {
                        reference: (*reference).into(),
                        action: (*action).into(),
                        actor: "Test".into(),
                        updated_at: "2026-10-20 12:00:00".into(),
                        show_reference: true,
                    }));
                    commit
                })
                .collect(),
        )
    }

    fn shown(app: &App) -> Vec<usize> {
        (0..app.commits.len())
            .filter(|&i| app.log_folds.visible(i))
            .collect()
    }

    #[test]
    fn rebase_folds_preserve_other_refs_and_independent_repeated_events() {
        let mut app = app(&[
            ("HEAD", "rebase (finish): returning to refs/heads/topic"),
            ("topic", "rebase (finish): refs/heads/topic onto abcdef0"),
            ("HEAD", "rebase (pick): first change"),
            ("other", "commit: unrelated"),
            ("HEAD", "rebase (start): checkout main"),
            ("HEAD", "checkout: moving from main to topic"),
            ("HEAD", "rebase -i (finish): returning to refs/heads/topic"),
            ("HEAD", "rebase -i (squash): older change"),
            ("HEAD", "rebase -i (start): checkout main"),
            ("HEAD", "commit: recovery point"),
        ]);
        assert_eq!(shown(&app), [0, 1, 3, 5, 6, 9]);
        assert_eq!(app.log_folds.marker(0), Some("▶"));
        assert_eq!(app.log_folds.label(0), " · 2 rebase steps");

        app.move_by(1, 1);
        assert_eq!(app.selected, 1);
        app.move_by(1, 1);
        assert_eq!(app.selected, 3);
        app.selected = 0;
        app.apply_log_action(LogAction::Fold);
        assert_eq!(shown(&app), [0, 1, 2, 3, 4, 5, 6, 9]);
        assert_eq!(app.log_folds.marker(0), Some("▼"));
        // Folding from an intermediate step returns to the finish entry.
        app.selected = 2;
        app.apply_log_action(LogAction::Fold);
        assert_eq!(app.selected, 0);
        assert_eq!(shown(&app), [0, 1, 3, 5, 6, 9]);

        app.search = Some("first change".into());
        app.next_match(false);
        assert_eq!(app.selected, 2);
        assert!(app.log_folds.visible(2));
        assert_eq!(app.log_folds.marker(0), Some("▼"));
        assert_eq!(app.log_folds.marker(6), Some("▶"));
        app.search = Some("older change".into());
        app.next_match(false);
        assert_eq!(app.selected, 7);
        assert!(app.log_folds.visible(7));
    }

    #[test]
    fn rebases_include_amended_commits_and_plain_fast_forward_events() {
        let events = [
            "rebase (finish): returning to refs/heads/fix/359-beat-synced-release-lag",
            "commit (amend): fix #359: tempo change in the current block",
            "rebase (pick): fix #359: tempo change from the next block",
            "commit (amend): test #359(failing): tempo change in the current block",
            "rebase (pick): test #359(failing): tempo change from the next block",
            "rebase (pick): refactor(test): group release parameters",
            "rebase (pick): refactor(test): extract processor setup",
            "commit (amend): fix: beats change without the UI timer",
            "rebase (pick): fix: beats change from the next block",
            "commit (amend): test #359(failing): beats change without the UI timer",
            "rebase (pick): test #359(failing): beats change from the next block",
            "commit (amend): fix: Recovery beats change without the UI timer",
            "rebase (pick): fix: Recovery beats change from the next block",
            "commit (amend): test #359(failing): Recovery beats change",
            "rebase: fast-forward",
            "rebase: fast-forward",
            "rebase (start): checkout origin/powair-v2-vibe",
            "commit (amend): standalone amendment before the rebase",
        ];
        let mut app = app(&events.iter().map(|e| ("HEAD", *e)).collect::<Vec<_>>());
        assert_eq!(shown(&app), [0, 17]);
        assert_eq!(app.log_folds.label(0), " · 16 rebase steps");
        app.search = Some("commit (amend): fix #359:".into());
        app.next_match(false);
        assert_eq!(app.selected, 1);
        assert_eq!(shown(&app).len(), events.len());
        app.apply_log_action(LogAction::Fold);
        assert_eq!(app.selected, 0);
        assert_eq!(shown(&app), [0, 17]);
        app.selected = 17;
        app.apply_log_action(LogAction::Fold);
        assert_eq!(shown(&app), [0, 17]);
        assert_eq!(app.log_folds.marker(17), None);
    }

    #[test]
    fn rebases_include_cherry_pick_commits_and_continued_steps() {
        let events = [
            "rebase (finish): returning to refs/heads/autocomp-make-up-store",
            "rebase (pick): refactor(test): count make-up gain writes with HostProbe",
            "rebase (pick): refactor(test): move HostProbe to the shared test helpers",
            "commit (cherry-pick): refactor(test): extract the make-up gain gesture probe",
            "rebase (pick): refactor(test): move the gesture probe to class scope",
            "rebase (continue): test(failing): a host change keeps the compensation",
            "rebase (continue): test: switching Auto on recomputes the compensation",
            "rebase (pick): refactor(test): extract recomputeThenPlayOn",
            "rebase (continue): test(failing): a UI change recomputes the compensation",
            "rebase (start): checkout origin/powair-v2-vibe",
            "commit (cherry-pick): standalone cherry-pick before the rebase",
        ];
        let mut app = app(&events.iter().map(|e| ("HEAD", *e)).collect::<Vec<_>>());
        assert_eq!(shown(&app), [0, 10]);
        assert_eq!(app.log_folds.label(0), " · 9 rebase steps");
        app.search = Some("commit (cherry-pick): refactor(test):".into());
        app.next_match(false);
        assert_eq!(app.selected, 3);
        assert_eq!(shown(&app).len(), events.len());
        app.apply_log_action(LogAction::Fold);
        assert_eq!(app.selected, 0);
        assert_eq!(shown(&app), [0, 10]);
        assert_eq!(app.log_folds.marker(10), None);
    }

    #[test]
    fn pull_rebases_fold_like_rebases() {
        let app = app(&[
            (
                "HEAD",
                "pull -r origin main -q (finish): returning to refs/heads/main",
            ),
            ("HEAD", "pull -r origin main -q (pick): local change"),
            (
                "HEAD",
                "pull -r origin main -q (start): checkout 3d9b31fc2f44",
            ),
            (
                "HEAD",
                "pull --rebase=interactive (finish): returning to refs/heads/main",
            ),
            (
                "HEAD",
                "pull --rebase=interactive (start): checkout 3d9b31fc2f44",
            ),
            ("HEAD", "commit: before the pulls"),
        ]);
        assert_eq!(shown(&app), [0, 3, 5]);
        assert_eq!(app.log_folds.label(0), " · 2 rebase steps");
        assert_eq!(app.log_folds.label(3), " · 1 rebase step");
    }

    #[test]
    fn amend_chains_keep_refs_and_intervening_actions_separate() {
        let mut app = app(&[
            ("HEAD", "commit (amend): newest version"),
            ("topic", "commit (amend): other ref's newest version"),
            ("HEAD", "commit (amend): intermediate version"),
            ("topic", "commit (initial): other ref's original"),
            ("HEAD", "commit: original version"),
            ("HEAD", "commit: previous commit stays visible"),
            ("HEAD", "commit (amend): amendment after checkout"),
            ("HEAD", "checkout: moving from topic to main"),
            ("HEAD", "commit: before checkout"),
            ("HEAD", "commit (amend): newest before reset"),
            ("HEAD", "commit (amend): oldest before reset"),
            ("HEAD", "reset: moving to HEAD~1"),
            ("HEAD", "commit: before reset"),
            ("HEAD", "commit (amend): lone truncated amendment"),
        ]);
        assert_eq!(shown(&app), [0, 1, 5, 6, 7, 8, 9, 11, 12, 13]);
        assert_eq!(app.log_folds.label(0), " · 2 earlier versions");
        assert_eq!(app.log_folds.label(1), " · 1 earlier version");
        assert_eq!(app.log_folds.label(9), " · 1 earlier version");
        assert_eq!(app.log_folds.marker(6), None);
        assert_eq!(app.log_folds.marker(13), None);
        app.move_by(1, 1);
        app.move_by(1, 1);
        assert_eq!(app.selected, 5);
        app.search = Some("commit: original version".into());
        app.next_match(false);
        assert_eq!(app.selected, 4);
        assert!(app.log_folds.visible(2));
        assert!(app.log_folds.visible(4));
        assert!(!app.log_folds.visible(3));
        assert_eq!(app.log_folds.label(0), "");
        app.apply_log_action(LogAction::Fold);
        assert_eq!(app.selected, 0);
        assert_eq!(shown(&app), [0, 1, 5, 6, 7, 8, 9, 11, 12, 13]);
    }

    #[test]
    fn amend_chains_include_cherry_picked_reverted_and_merged_originals() {
        let app = app(&[
            ("HEAD", "commit (amend): amended cherry-pick"),
            ("HEAD", "cherry-pick: picked"),
            ("HEAD", "commit (amend): amended revert"),
            ("HEAD", "revert: Revert \"picked\""),
            ("HEAD", "commit (amend): amended merge"),
            ("HEAD", "merge side: Merge made by the 'ort' strategy."),
        ]);
        assert_eq!(shown(&app), [0, 2, 4]);
        for row in [0, 2, 4] {
            assert_eq!(app.log_folds.label(row), " · 1 earlier version");
        }
    }

    #[test]
    fn truncated_amend_chains_fold_only_the_available_versions() {
        let mut app = app(&[
            ("HEAD", "commit (amend): newest"),
            ("HEAD", "commit (amend): oldest available"),
        ]);
        assert_eq!(shown(&app), [0]);
        assert_eq!(app.log_folds.label(0), " · 1 earlier version");
        app.apply_log_action(LogAction::Fold);
        assert_eq!(shown(&app), [0, 1]);
    }

    #[test]
    fn amendments_inside_rebases_have_no_separate_folds() {
        let mut app = app(&[
            ("HEAD", "rebase (finish): returning to refs/heads/topic"),
            ("HEAD", "commit (amend): latest edit"),
            ("HEAD", "commit (amend): earlier edit"),
            ("HEAD", "rebase (pick): original"),
            ("HEAD", "rebase (start): checkout main"),
            ("HEAD", "commit: before rebase"),
        ]);
        assert_eq!(shown(&app), [0, 5]);
        app.apply_log_action(LogAction::Fold);
        assert_eq!(shown(&app), [0, 1, 2, 3, 4, 5]);
        assert_eq!(app.log_folds.marker(1), None);
        app.selected = 2;
        app.apply_log_action(LogAction::Fold);
        assert_eq!(app.selected, 0);
        assert_eq!(shown(&app), [0, 5]);
    }

    #[test]
    fn aborted_rebases_fold_at_the_abort_without_hiding_other_operations() {
        let mut app = app(&[
            (
                "HEAD",
                "rebase (abort): returning to refs/heads/autocomp-make-up-store",
            ),
            (
                "HEAD",
                "rebase (pick): refactor: loadInternalParameters takes the state as const",
            ),
            ("other", "commit: unrelated ref update"),
            ("HEAD", "commit (amend): abandoned edit"),
            ("HEAD", "commit (amend): abandoned earlier edit"),
            ("HEAD", "rebase (start): checkout powair-v2-vibe"),
            ("HEAD", "commit: before the aborted rebase"),
            ("HEAD", "rebase (finish): returning to refs/heads/topic"),
            ("HEAD", "rebase (pick): earlier successful rebase"),
            ("HEAD", "rebase (start): checkout main"),
            ("HEAD", "commit: before the successful rebase"),
        ]);
        assert_eq!(shown(&app), [0, 2, 6, 7, 10]);
        assert_eq!(app.log_folds.marker(0), Some("▶"));
        assert_eq!(app.log_folds.label(0), " · 4 rebase steps");
        app.apply_log_action(LogAction::Fold);
        assert_eq!(shown(&app), [0, 1, 2, 3, 4, 5, 6, 7, 10]);
        assert_eq!(app.log_folds.marker(3), None);
        app.selected = 4;
        app.apply_log_action(LogAction::Fold);
        assert_eq!(app.selected, 0);
        assert_eq!(shown(&app), [0, 2, 6, 7, 10]);
        app.search = Some("loadInternalParameters".into());
        app.next_match(false);
        assert_eq!(app.selected, 1);
        assert!(app.log_folds.visible(1));
        assert_eq!(app.log_folds.marker(7), Some("▶"));
    }

    #[test]
    fn incomplete_or_interrupted_rebases_stay_expanded() {
        for events in [
            vec!["rebase (pick): ongoing", "rebase (start): checkout main"],
            vec!["commit (amend): ongoing", "rebase (start): checkout main"],
            vec![
                "commit (amend): newest ongoing edit",
                "commit (amend): previous ongoing edit",
                "rebase (start): checkout main",
            ],
            vec![
                "commit (cherry-pick): ongoing",
                "rebase (start): checkout main",
            ],
            vec![
                "rebase (finish): returning to refs/heads/topic",
                "commit (amend): truncated",
                "rebase: fast-forward",
            ],
            vec![
                "rebase (finish): returning to refs/heads/topic",
                "rebase (pick): truncated",
            ],
            vec![
                "rebase (abort): returning to refs/heads/topic",
                "rebase (pick): abandoned",
            ],
            vec![
                "rebase (abort): returning to refs/heads/topic",
                "checkout: moving from main to topic",
                "rebase (start): checkout main",
            ],
            vec![
                "rebase (finish): returning to refs/heads/topic",
                "checkout: moving from main to topic",
                "rebase (start): checkout main",
            ],
            vec![
                "rebase (finish): returning to refs/heads/topic",
                "rebase (unknown): unexpected",
                "rebase (start): checkout main",
            ],
        ] {
            let mut app = app(&events.iter().map(|e| ("HEAD", *e)).collect::<Vec<_>>());
            assert_eq!(shown(&app).len(), events.len(), "{events:?}");
            app.apply_log_action(LogAction::Fold);
            assert_eq!(shown(&app).len(), events.len(), "{events:?}");
            assert!(app.status.is_none());
            assert_eq!(app.log_folds.marker(0), None);
        }
        let app = app(&[
            ("topic", "rebase (finish): returning to refs/heads/topic"),
            ("HEAD", "rebase (start): checkout main"),
        ]);
        assert_eq!(shown(&app), [0, 1]);
    }
}
