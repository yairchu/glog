//! Optional GitHub annotations. Network reads never hold up the branch picker.
use std::{
    collections::{BTreeSet, HashMap},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

use ratatui::style::Color;
use serde::Deserialize;

use crate::git::{self, Annotation, Commit, Repository};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
enum Status {
    Open,
    Merged,
    Closed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Badge {
    number: u64,
    state: Status,
    is_draft: bool,
    url: String,
}

impl Badge {
    pub(super) fn url(&self) -> &str {
        &self.url
    }
    pub(super) fn label(&self) -> String {
        let state = match self.state {
            Status::Open if self.is_draft => "draft",
            Status::Open => "open",
            Status::Merged => "merged",
            Status::Closed => "closed",
        };
        format!("#{} {state}", self.number)
    }

    pub(super) fn color(&self) -> Color {
        match self.state {
            Status::Open if self.is_draft => Color::Gray,
            Status::Open => Color::Green,
            Status::Merged => Color::Magenta,
            Status::Closed => Color::Red,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    #[serde(flatten)]
    badge: Badge,
    head_ref_name: String,
    head_repository: Option<HeadRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadRepository {
    name_with_owner: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Head {
    repository: String,
    branch: String,
}

type Badges = HashMap<String, Badge>;

#[derive(Default)]
pub(crate) struct State {
    pending: Option<Receiver<Option<Badges>>>,
    checked: Option<Instant>,
    badges: Badges,
}

impl State {
    pub(crate) fn loading(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn poll(&mut self, rows: &mut [Commit], watch: bool) -> bool {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(result) => {
                    if let Some(badges) = result {
                        self.badges = badges;
                    }
                    self.pending = None;
                    self.checked = Some(Instant::now());
                }
                Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.checked = Some(Instant::now());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let mut changed = false;
        for row in rows {
            if let Some(Annotation::Branch(node)) = &mut row.annotation {
                let badges: Vec<_> = node
                    .branches
                    .iter()
                    .filter(|branch| branch.kind == super::BranchKind::Local)
                    .filter_map(|branch| {
                        Some((branch.name.clone(), self.badges.get(&branch.name)?.clone()))
                    })
                    .collect();
                changed |= node.pull_requests != badges;
                node.pull_requests = badges;
            }
        }
        if self.pending.is_none()
            && self
                .checked
                .is_none_or(|checked| watch && checked.elapsed() >= Duration::from_secs(60))
        {
            self.checked = Some(Instant::now());
            if let Ok(repository) = Repository::main() {
                let (sender, receiver) = mpsc::channel();
                self.pending = Some(receiver);
                std::thread::spawn(move || {
                    let _ = sender.send(load(&repository));
                });
            }
        }
        changed
    }
}

pub(crate) fn selected_url(commit: &Commit) -> Option<String> {
    let node = commit.branch()?;
    node.branches
        .iter()
        .filter(|branch| branch.current)
        .chain(node.branches.iter().filter(|branch| !branch.current))
        .find_map(|branch| {
            node.pull_requests
                .iter()
                .find(|(name, _)| *name == branch.name)
                .map(|(_, badge)| badge.url.clone())
        })
}

fn remotes(repository: &Repository) -> Option<HashMap<String, String>> {
    let output = git::read_command_at(Some(repository))
        .args(["remote"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut remotes = HashMap::new();
    for remote in String::from_utf8(output.stdout).ok()?.lines() {
        // Preserve GitHub identity when transport URLs are rewritten locally.
        let stored = git::read_command_at(Some(repository))
            .args(["config", "--get", &format!("remote.{remote}.url")])
            .output()
            .ok()?;
        let url = git::github_repository_url(String::from_utf8_lossy(&stored.stdout).trim())
            .or_else(|| {
                let resolved = git::read_command_at(Some(repository))
                    .args(["remote", "get-url", remote])
                    .output()
                    .ok()?;
                git::github_repository_url(String::from_utf8_lossy(&resolved.stdout).trim())
            });
        if let Some(url) = url {
            remotes.insert(remote.to_owned(), url);
        }
    }
    Some(remotes)
}

fn heads(
    repository: &Repository,
    remotes: &HashMap<String, String>,
) -> Option<HashMap<String, Head>> {
    let output = git::read_command_at(Some(repository))
        .args([
            "for-each-ref",
            "--format=%(refname:strip=2)%00%(upstream:remotename)%00%(upstream:remoteref)",
            "refs/heads/",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut heads = HashMap::new();
    for line in String::from_utf8(output.stdout).ok()?.lines() {
        let fields: Vec<_> = line.split('\0').collect();
        let [local, remote, reference] = fields.as_slice() else {
            return None;
        };
        let (remote, branch) = if remote.is_empty() {
            ("origin", *local)
        } else {
            let Some(branch) = reference.strip_prefix("refs/heads/") else {
                continue;
            };
            (*remote, branch)
        };
        if let Some(repository) = remotes.get(remote) {
            heads.insert(
                (*local).to_owned(),
                Head {
                    repository: repository.clone(),
                    branch: branch.to_owned(),
                },
            );
        }
    }
    Some(heads)
}

fn matching_badge(output: &[u8], head: &Head, base: &str) -> Option<Option<Badge>> {
    let prs: Vec<PullRequest> = serde_json::from_slice(output).ok()?;
    Some(
        prs.into_iter()
            .filter(|pr| {
                pr.head_ref_name == head.branch
                    && pr.head_repository.as_ref().is_some_and(|repository| {
                        head.repository.eq_ignore_ascii_case(&format!(
                            "https://github.com/{}",
                            repository.name_with_owner
                        ))
                    })
                    && pr
                        .badge
                        .url
                        .eq_ignore_ascii_case(&format!("{base}/pull/{}", pr.badge.number))
            })
            // An open PR takes precedence when a branch name was reused.
            .max_by_key(|pr| (pr.badge.state == Status::Open, pr.badge.number))
            .map(|pr| pr.badge),
    )
}

fn load(repository: &Repository) -> Option<Badges> {
    load_with(repository, |base, head| {
        let output = Command::new("gh")
            .current_dir(repository.directory())
            .args(["pr", "list", "--repo", base, "--head", &head.branch])
            .args([
                "--state",
                "all",
                "--limit",
                "100",
                "--json",
                "number,state,isDraft,url,headRefName,headRepository",
            ])
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output.status.success().then_some(output.stdout)
    })
}

fn load_with(
    repository: &Repository,
    mut query: impl FnMut(&str, &Head) -> Option<Vec<u8>>,
) -> Option<Badges> {
    let remotes = remotes(repository)?;
    let heads = heads(repository, &remotes)?;
    let bases: BTreeSet<_> = remotes.values().collect();
    let unique: BTreeSet<_> = heads.values().cloned().collect();
    let mut found = HashMap::new();
    let mut successful = unique.is_empty();
    for head in unique {
        let mut matches = Vec::new();
        // Searching configured base repositories also finds PRs from a fork
        // (origin) into its upstream. Verify the head repository, not just its name.
        for base in &bases {
            let Some(output) = query(base, &head) else {
                continue;
            };
            let Some(badge) = matching_badge(&output, &head, base) else {
                continue;
            };
            successful = true;
            if let Some(badge) = badge {
                matches.push(badge);
            }
        }
        if let Some(badge) = matches
            .into_iter()
            .max_by_key(|badge| (badge.state == Status::Open, badge.number))
        {
            for (local, candidate) in &heads {
                if *candidate == head {
                    found.insert(local.clone(), badge.clone());
                }
            }
        }
    }
    successful.then_some(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn keyboard_and_badge_clicks_open_the_correct_pr() {
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };
        use ratatui::{backend::TestBackend, style::Modifier, Terminal};
        use std::{env, fs, os::unix::fs::PermissionsExt, process::Command};
        const CHILD: &str = "GLOG_TEST_BRANCH_PR_BROWSER";
        const CAPTURE: &str = "GLOG_TEST_BRANCH_PR_CAPTURE";

        if env::var_os(CHILD).is_some() {
            let directory = TestDirectory::new();
            let _cwd = CurrentDirGuard::enter(directory.path());
            git(&["init", "-qb", "main"]);
            git(&[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ]);
            git(&["branch", "界-topic"]);
            let mut app = crate::branches::load_picker().unwrap();
            let Some(Annotation::Branch(node)) = &mut app.commits[0].annotation else {
                panic!("branch node")
            };
            node.pull_requests = [("main", 12), ("界-topic", 11)]
                .into_iter()
                .map(|(name, number)| {
                    (
                        name.into(),
                        Badge {
                            number,
                            state: Status::Open,
                            is_draft: false,
                            url: format!("https://github.com/base/repo/pull/{number}"),
                        },
                    )
                })
                .collect();
            let mut terminal = Terminal::new(TestBackend::new(160, 10)).unwrap();
            app.search = Some("open".into());
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            assert_eq!(app.visible_log_urls.len(), 2);
            let (rect, _) = app
                .visible_log_urls
                .iter()
                .find(|(_, url)| url.ends_with("/11"))
                .unwrap()
                .clone();
            assert!(terminal.backend().buffer()[(rect.x, rect.y)]
                .modifier
                .contains(Modifier::UNDERLINED));
            assert_eq!(terminal.backend().buffer()[(rect.x, rect.y)].symbol(), "#");
            let capture = env::var_os(CAPTURE).unwrap();
            let wait_for_url = |app: &mut crate::app::App, number| {
                let expected = format!("https://github.com/base/repo/pull/{number}");
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    app.poll_browser();
                    if fs::read_to_string(&capture).is_ok_and(|text| text.trim() == expected) {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "browser was not opened for {expected}; status: {:?}",
                        app.status
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert_eq!(app.history, crate::app::History::Branches);
                assert_eq!(app.mode, crate::app::Mode::Log);
            };
            crate::input::handle(
                Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
                &mut app,
            );
            wait_for_url(&mut app, 12);
            crate::input::handle(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut app,
            );
            wait_for_url(&mut app, 11);

            // Hit boxes include only the visible part of a badge, using display
            // columns rather than UTF-8 bytes after the Unicode branch name.
            terminal.backend_mut().resize(rect.x + 2, 10);
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let (clipped, _) = app
                .visible_log_urls
                .iter()
                .find(|(_, url)| url.ends_with("/11"))
                .unwrap();
            assert_eq!(clipped.width, 2);
            terminal.backend_mut().resize(rect.x, 10);
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            assert!(app
                .visible_log_urls
                .iter()
                .all(|(_, url)| !url.ends_with("/11")));
            return;
        }

        let directory = TestDirectory::new();
        let opener = directory.path().join(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        });
        fs::write(&opener, "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$GLOG_TEST_BRANCH_PR_CAPTURE.tmp\"\nmv \"$GLOG_TEST_BRANCH_PR_CAPTURE.tmp\" \"$GLOG_TEST_BRANCH_PR_CAPTURE\"\n").unwrap();
        fs::set_permissions(&opener, fs::Permissions::from_mode(0o755)).unwrap();
        let original = env::var_os("PATH").unwrap();
        let path = env::join_paths(
            std::iter::once(directory.path().to_path_buf()).chain(env::split_paths(&original)),
        )
        .unwrap();
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "branches::pull_requests::tests::keyboard_and_badge_clicks_open_the_correct_pr",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(CAPTURE, directory.path().join("opened-url"))
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    fn pr(number: u64, owner: &str, branch: &str, state: &str, draft: bool) -> serde_json::Value {
        json!({
            "number": number, "state": state, "isDraft": draft,
            "url": format!("https://github.com/base/repo/pull/{number}"),
            "headRefName": branch, "headRepository": {"nameWithOwner": owner}
        })
    }

    #[test]
    fn reused_branch_names_prefer_open_prs_and_verify_fork_identity() {
        let head = Head {
            repository: "https://github.com/me/repo".into(),
            branch: "topic".into(),
        };
        let prs = vec![
            pr(1, "ME/repo", "topic", "OPEN", true),
            pr(2, "me/repo", "topic", "MERGED", false),
            pr(3, "other/repo", "topic", "OPEN", false),
            pr(4, "me/repo", "different", "OPEN", false),
        ];
        let parse = |prs: &[serde_json::Value]| {
            matching_badge(
                &serde_json::to_vec(prs).unwrap(),
                &head,
                "https://github.com/base/repo",
            )
            .unwrap()
            .unwrap()
        };
        assert_eq!(parse(&prs).label(), "#1 draft");
        assert_eq!(
            matching_badge(
                &serde_json::to_vec(&prs).unwrap(),
                &head,
                "https://github.com/Base/Repo"
            )
            .unwrap()
            .unwrap()
            .label(),
            "#1 draft"
        );
        assert_eq!(parse(&prs[1..]).label(), "#2 merged");
        let mut deleted = prs[0].clone();
        deleted["headRepository"] = serde_json::Value::Null;
        assert!(matching_badge(
            &serde_json::to_vec(&vec![deleted]).unwrap(),
            &head,
            "https://github.com/base/repo"
        )
        .unwrap()
        .is_none());
        assert!(matching_badge(b"invalid", &head, "https://github.com/base/repo").is_none());
    }

    #[test]
    fn upstream_renames_and_forks_match_without_network_and_survive_row_reload() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ]);
        git(&["branch", "local-topic"]);
        git(&["branch", "topic"]);
        git(&["remote", "add", "origin", "git@github.com:me/repo.git"]);
        git(&[
            "remote",
            "add",
            "upstream",
            "https://github.com/base/repo.git",
        ]);
        git(&["config", "branch.local-topic.remote", "origin"]);
        git(&["config", "branch.local-topic.merge", "refs/heads/topic"]);
        // Tracking refs can be gone after a PR was merged and its branch deleted.
        let repository = Repository::main().unwrap();
        let badges = load_with(&repository, |base, head| {
            if base != "https://github.com/base/repo" {
                return None; // An inaccessible remote must not hide another remote's PRs.
            }
            Some(
                serde_json::to_vec(&if head.branch == "topic" {
                    vec![pr(10, "me/repo", "topic", "OPEN", false)]
                } else {
                    vec![]
                })
                .unwrap(),
            )
        })
        .unwrap();
        assert_eq!(badges.len(), 2);
        assert_eq!(badges["local-topic"].label(), "#10 open");
        assert_eq!(badges["topic"].label(), "#10 open");

        let mut app = crate::branches::load_picker().unwrap();
        let (sender, receiver) = mpsc::channel();
        app.branch_pull_requests = State {
            pending: Some(receiver),
            ..Default::default()
        };
        assert!(!app.branch_pull_requests.poll(&mut app.commits, false));
        sender.send(Some(badges)).unwrap();
        assert!(app.branch_pull_requests.poll(&mut app.commits, false));
        let rendered: String = crate::branches::summary(&app)
            .into_iter()
            .map(|line| line.to_string())
            .collect();
        assert!(rendered.contains("local-topic #10 open"));
        assert!(rendered.contains("topic #10 open"));
        assert_eq!(
            selected_url(&app.commits[0]),
            Some("https://github.com/base/repo/pull/10".into())
        );
        app.commits = crate::branches::load_watched(&[]).unwrap();
        assert!(app.branch_pull_requests.poll(&mut app.commits, false));
        assert!(!app.branch_pull_requests.poll(&mut app.commits, false));
        let (sender, receiver) = mpsc::channel();
        app.branch_pull_requests.pending = Some(receiver);
        sender.send(None).unwrap();
        assert!(!app.branch_pull_requests.poll(&mut app.commits, false));
        let (sender, receiver) = mpsc::channel();
        app.branch_pull_requests.pending = Some(receiver);
        sender.send(Some(HashMap::new())).unwrap();
        assert!(app.branch_pull_requests.poll(&mut app.commits, false));
        assert!(selected_url(&app.commits[0]).is_none());
    }
}
