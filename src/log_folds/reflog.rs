//! Reflog folds describe operations; successive rebases can form a chain
//! when their targets advance along the same ancestry path.
use std::collections::{BTreeMap, HashMap};

use crate::git::{self, Annotation, Commit};

use super::{Fold, Layout};

#[derive(Default)]
pub(super) struct Folds {
    groups: BTreeMap<usize, Group>,
    chains: Vec<Chain>,
}

#[derive(PartialEq, Eq)]
enum Kind {
    Rebase,
    Amend,
}

struct Chain {
    // Individual rebase headers, newest first. The newest header also
    // represents the chain so opening a commit keeps its usual meaning.
    headers: Vec<usize>,
    expanded: bool,
}

struct Group {
    kind: Kind,
    // Newest first, with the entry representing the fold at the head.
    // Entries from other refs may be interleaved in `reflog --all`.
    rows: Vec<usize>,
    expanded: bool,
}

impl Folds {
    pub(super) fn new(commits: &[Commit], repository: Option<&git::Repository>) -> Self {
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
        // Also leave amendments in a rebase Git still has in progress alone.
        // HEAD's newest entries belong to it, back to its start, whatever
        // their actions, and all of them when a count limit omits the start.
        // Tools can name the rebase with GIT_REFLOG_ACTION, but Git still
        // marks its start, e.g. "autofix (start)".
        for (index, commit) in commits.iter().enumerate() {
            let Some(Annotation::Reflog(entry)) = &commit.annotation else {
                continue;
            };
            if !entry.rebasing {
                continue;
            }
            if matches!(rebase_event(&entry.action), Some(RebaseEvent::End)) {
                break;
            }
            in_rebase[index] = true;
            let started = entry
                .action
                .split_once(": ")
                .is_some_and(|(stage, _)| stage.ends_with(" (start)"));
            if started {
                break;
            }
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
        let chains = rebase_chains(commits, &groups, |old, new| {
            git::read_command_at(repository)
                .args(["merge-base", "--is-ancestor", old, new])
                .env("GIT_NO_LAZY_FETCH", "1")
                .output()
                .is_ok_and(|output| output.status.success())
        });
        Self { groups, chains }
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
        // Apply outer folds last: every hidden row must point directly to
        // its visible owner, including the steps of folded child rebases.
        for chain in self.chains.iter().filter(|chain| !chain.expanded) {
            let header = chain.headers[0];
            let mut hidden = 0;
            for child in &chain.headers {
                for &row in &self.groups[child].rows {
                    if row != header {
                        layout.visible[row] = false;
                        layout.owners[row] = Some(header);
                        hidden += 1;
                    }
                }
            }
            layout.folds[header] = Fold::Folded(hidden);
        }
        layout
    }

    pub(super) fn contains(&self, index: usize) -> bool {
        self.groups.values().any(|g| g.rows.contains(&index))
    }

    pub(super) fn toggle(&mut self, index: usize) {
        if let Some(chain) = self.chains.iter_mut().find(|c| c.headers[0] == index) {
            let latest = self.groups.get_mut(&index).unwrap();
            // Parent and newest child share a header. Cycle through both
            // levels without adding a navigation key or a synthetic commit.
            if !chain.expanded {
                chain.expanded = true;
            } else if !latest.expanded {
                latest.expanded = true;
            } else {
                latest.expanded = false;
                chain.expanded = false;
            }
            return;
        }
        if let Some(group) = self.groups.values_mut().find(|g| g.rows.contains(&index)) {
            group.expanded = !group.expanded;
        }
    }

    pub(super) fn reveal(&mut self, index: usize) {
        for chain in &mut self.chains {
            if index != chain.headers[0]
                && chain
                    .headers
                    .iter()
                    .any(|h| self.groups[h].rows.contains(&index))
            {
                chain.expanded = true;
            }
        }
        if let Some(group) = self
            .groups
            .values_mut()
            .find(|g| g.rows[1..].contains(&index))
        {
            group.expanded = true;
        }
    }

    pub(super) fn label(&self, index: usize) -> String {
        if let Some(chain) = self.chains.iter().find(|c| c.headers[0] == index) {
            let mut label = format!(" · {} rebases", chain.headers.len());
            if chain.expanded {
                label.push_str(if self.groups[&index].expanded {
                    " · z: fold chain"
                } else {
                    " · z: latest steps"
                });
            }
            return label;
        }
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

/// Match only complete, successful operations adjacent on the same ref.
/// Other refs may interleave in --all without becoming part of the fold.
fn rebase_chains(
    commits: &[Commit],
    groups: &BTreeMap<usize, Group>,
    mut is_ancestor: impl FnMut(&str, &str) -> bool,
) -> Vec<Chain> {
    let mut next_on_ref = vec![None; commits.len()];
    let mut next = HashMap::new();
    for (index, commit) in commits.iter().enumerate().rev() {
        if let Some(Annotation::Reflog(entry)) = &commit.annotation {
            next_on_ref[index] = next.insert(entry.reference.as_str(), index);
        }
    }
    let branch = |header: usize| {
        let Some(Annotation::Reflog(entry)) = &commits[header].annotation else {
            return None;
        };
        let (action, body) = entry.action.split_once(": ")?;
        (rebase_stage(action) == Some("finish"))
            .then(|| body.strip_prefix("returning to refs/heads/"))
            .flatten()
    };
    let mut chains = Vec::new();
    let mut included = std::collections::HashSet::new();
    let mut ancestry = HashMap::new();
    for (&header, group) in groups {
        if group.kind != Kind::Rebase || included.contains(&header) {
            continue;
        }
        let Some(branch_name) = branch(header) else {
            continue;
        };
        let mut headers = vec![header];
        let mut newer = group;
        while let Some(older_header) = next_on_ref[*newer.rows.last().unwrap()] {
            let Some(older) = groups.get(&older_header).filter(|g| g.kind == Kind::Rebase) else {
                break;
            };
            if branch(older_header) != Some(branch_name) {
                break;
            }
            // Start entries pin the targets, even when Git recorded a mutable
            // branch name in the action. Equality is not forward progress.
            let old = commits[*older.rows.last().unwrap()].hash.as_str();
            let new = commits[*newer.rows.last().unwrap()].hash.as_str();
            if old == new
                || !*ancestry
                    .entry((old, new))
                    .or_insert_with(|| is_ancestor(old, new))
            {
                break;
            }
            headers.push(older_header);
            included.insert(older_header);
            newer = older;
        }
        if headers.len() > 1 {
            chains.push(Chain {
                headers,
                expanded: false,
            });
        }
    }
    chains
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
        // "merge <revision>" or "pull [<options>]", either of which can also
        // be a fast-forward. Rebasing pulls have stages, e.g. "pull (pick)".
        action if action.starts_with("merge ") => Some(CommitEvent::Original),
        action
            if matches!(action.split(' ').next(), Some("pull"))
                && rebase_stage(action).is_none() =>
        {
            Some(CommitEvent::Original)
        }
        _ => None,
    }
}

enum RebaseEvent {
    Start,
    End,
    Step,
}

fn rebase_event(action: &str) -> Option<RebaseEvent> {
    // Replaying or editing commits can produce ordinary commit and reset
    // actions, e.g. when splitting a commit. These count as steps only
    // inside a matched start/end sequence.
    if commit_event(action).is_some()
        || action == "rebase: fast-forward"
        || action.starts_with("reset: ")
    {
        return Some(RebaseEvent::Step);
    }
    match rebase_stage(action.split_once(": ")?.0)? {
        "start" => Some(RebaseEvent::Start),
        "finish" | "abort" => Some(RebaseEvent::End),
        "pick" | "reword" | "edit" | "squash" | "fixup" | "continue" | "reset" | "merge"
        | "skip" | "fast-forward" => Some(RebaseEvent::Step),
        _ => None,
    }
}

/// The stage of a rebase action, the part before ": ", e.g. "pick".
pub fn rebase_stage(action: &str) -> Option<&str> {
    // Stages follow the command line, e.g. "rebase -i (pick)" in older Git
    // versions, or "pull -r origin main (pick)". A continued rebase can have
    // more than one parenthesized stage, e.g. (continue) (finish).
    let (command, _) = action.split_once(" (")?;
    if !matches!(command.split(' ').next(), Some("rebase" | "pull")) {
        return None;
    }
    action.rsplit_once(" (")?.1.strip_suffix(')')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{App, LogAction},
        git::ReflogEntry,
    };

    fn app(events: &[(&str, &str)]) -> App {
        app_during_rebase(events, false)
    }

    fn app_during_rebase(events: &[(&str, &str)], rebasing: bool) -> App {
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
                        rebasing: rebasing && *reference == "HEAD",
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
    fn rebase_chains_stop_at_aborts_branch_changes_and_other_actions() {
        for (newest, between, oldest) in [
            (
                "rebase (abort): returning to refs/heads/topic",
                None,
                "rebase (finish): returning to refs/heads/topic",
            ),
            (
                "rebase (finish): returning to refs/heads/topic",
                None,
                "rebase (abort): returning to refs/heads/topic",
            ),
            (
                "rebase (finish): returning to refs/heads/topic",
                None,
                "rebase (finish): returning to refs/heads/other",
            ),
            (
                "rebase (finish): returning to refs/heads/topic",
                Some("checkout: moving from topic to other"),
                "rebase (finish): returning to refs/heads/topic",
            ),
            (
                "rebase (finish): returning to refs/heads/topic",
                Some("commit: independent change"),
                "rebase (finish): returning to refs/heads/topic",
            ),
            (
                "rebase (finish): returning to refs/heads/topic",
                Some("reset: moving to HEAD~1"),
                "rebase (finish): returning to refs/heads/topic",
            ),
        ] {
            let mut events = vec![
                newest,
                "rebase (pick): latest",
                "rebase (start): checkout main",
            ];
            events.extend(between);
            events.extend([
                oldest,
                "rebase (pick): earlier",
                "rebase (start): checkout main",
            ]);
            let mut app = app(&events
                .iter()
                .map(|event| ("HEAD", *event))
                .collect::<Vec<_>>());
            // Give the two starts distinct targets. Even with proven ancestry,
            // the operation boundaries above must prevent chain formation.
            let last = app.commits.len() - 1;
            app.commits[2].hash = "a".repeat(40);
            app.commits[last].hash = "b".repeat(40);
            let groups = &app.log_folds.reflog.as_ref().unwrap().groups;
            assert!(
                rebase_chains(&app.commits, groups, |_, _| true).is_empty(),
                "{events:?}"
            );
        }
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
    fn rebases_include_resets_that_split_commits() {
        let app = app(&[
            ("HEAD", "rebase (finish): returning to refs/heads/main"),
            ("HEAD", "commit: second half"),
            ("HEAD", "commit: first half"),
            ("HEAD", "reset: moving to HEAD^"),
            ("HEAD", "rebase: fast-forward"),
            ("HEAD", "rebase (start): checkout HEAD~2"),
            ("HEAD", "commit: before the rebase"),
        ]);
        assert_eq!(shown(&app), [0, 6]);
        assert_eq!(app.log_folds.label(0), " · 5 rebase steps");
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
            ("HEAD", "commit (amend): amended pull"),
            (
                "HEAD",
                "pull origin main: Merge made by the 'ort' strategy.",
            ),
            ("HEAD", "commit (amend): amended fast-forward pull"),
            ("HEAD", "pull: Fast-forward"),
        ]);
        assert_eq!(shown(&app), [0, 2, 4, 6, 8]);
        for row in [0, 2, 4, 6, 8] {
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
            // A count limit can omit the start of the ongoing rebase.
            vec![
                "commit (amend): newest ongoing edit",
                "commit (amend): previous ongoing edit",
            ],
            vec![
                "reset: moving to HEAD",
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
            let events: Vec<_> = events.iter().map(|e| ("HEAD", *e)).collect();
            let mut app = app_during_rebase(&events, true);
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

    #[test]
    fn ongoing_rebases_started_by_other_tools_leave_older_amendments_folded() {
        // Tools can name their rebases with GIT_REFLOG_ACTION.
        let app = app_during_rebase(
            &[
                ("HEAD", "commit (amend): newest ongoing edit"),
                ("HEAD", "commit (amend): previous ongoing edit"),
                ("HEAD", "autofix (start): checkout main"),
                ("HEAD", "commit (amend): before the rebase"),
                ("HEAD", "commit: original before the rebase"),
            ],
            true,
        );
        assert_eq!(shown(&app), [0, 1, 2, 3]);
        assert_eq!(app.log_folds.label(3), " · 1 earlier version");
    }
}
