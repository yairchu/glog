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

struct Remote {
    fetch: Option<String>,
    push: Option<String>,
}

fn remote_repository(repository: &Repository, remote: &str, push: bool) -> Option<String> {
    let stored_url = |key| {
        git::read_command_at(Some(repository))
            .args(["config", "--get", &format!("remote.{remote}.{key}")])
            .output()
            .ok()
            .filter(|output| output.status.success())
    };
    // An explicit non-GitHub push URL must not inherit the fetch identity.
    let stored = push
        .then(|| stored_url("pushurl"))
        .flatten()
        .or_else(|| stored_url("url"));
    // Preserve GitHub identity when transport URLs are rewritten locally.
    stored
        .and_then(|output| {
            git::github_repository_url(String::from_utf8_lossy(&output.stdout).trim())
        })
        .or_else(|| {
            let mut command = git::read_command_at(Some(repository));
            command.args(["remote", "get-url"]);
            if push {
                command.arg("--push");
            }
            let resolved = command.arg(remote).output().ok()?;
            if !resolved.status.success() {
                return None;
            }
            git::github_repository_url(String::from_utf8_lossy(&resolved.stdout).trim())
        })
}

fn remotes(repository: &Repository) -> Option<HashMap<String, Remote>> {
    let output = git::read_command_at(Some(repository))
        .args(["remote"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8(output.stdout)
            .ok()?
            .lines()
            .map(|remote| {
                (
                    remote.to_owned(),
                    Remote {
                        fetch: remote_repository(repository, remote, false),
                        push: remote_repository(repository, remote, true),
                    },
                )
            })
            .collect(),
    )
}

fn heads(
    repository: &Repository,
    remotes: &HashMap<String, Remote>,
) -> Option<HashMap<String, Head>> {
    let heads = super::push_target::load(Some(repository))
        .ok()?
        .into_iter()
        .filter_map(|(local, target)| {
            let repository = remotes.get(&target.remote)?.push.clone()?;
            Some((
                local,
                Head {
                    repository,
                    branch: target.branch,
                },
            ))
        })
        .collect();
    Some(heads)
}

/// The `owner/name` of a GitHub repository URL from [`git::github_repository_url`].
fn owner_and_name(url: &str) -> Option<&str> {
    url.strip_prefix("https://github.com/")
}

#[derive(Deserialize)]
struct Response {
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    repository: BaseRepository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaseRepository {
    /// Canonical, following renames and transfers that remote URLs predate.
    name_with_owner: String,
    #[serde(flatten)]
    heads: HashMap<String, Connection>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    nodes: Vec<PullRequest>,
    page_info: PageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

type Cursors<'a> = HashMap<&'a str, String>;

/// Branches looked up per GraphQL request.
const BATCH: usize = 50;

/// One GraphQL query finds the PRs of a batch of head branch names, passed as
/// variables `$h0`, `$h1`, ... alongside `$owner` and `$name`.
fn graphql_query(count: usize) -> String {
    let variables: String = (0..count)
        .map(|i| format!(",$h{i}:String!,$c{i}:String"))
        .collect();
    let fields: String = (0..count)
        .map(|i| {
            format!(
                " h{i}:pullRequests(headRefName:$h{i},states:[OPEN,CLOSED,MERGED],first:100,after:$c{i})\
                 {{nodes{{number state isDraft url headRefName headRepository{{nameWithOwner}}}}pageInfo{{hasNextPage endCursor}}}}"
            )
        })
        .collect();
    format!(
        "query($owner:String!,$name:String!{variables})\
         {{repository(owner:$owner,name:$name){{nameWithOwner{fields}}}}}"
    )
}

/// The canonical name of `base` and its PRs from any of `branches`, or `None`
/// when any request fails.
fn base_pull_requests(
    base: &str,
    branches: &[&str],
    query: &mut impl FnMut(&str, &[&str], &Cursors<'_>) -> Option<Vec<u8>>,
) -> Option<(String, Vec<PullRequest>)> {
    let mut name = None;
    let mut prs = Vec::new();
    for batch in branches.chunks(BATCH) {
        let mut pending = batch.to_vec();
        let mut cursors = Cursors::new();
        while !pending.is_empty() {
            let response: Response =
                serde_json::from_slice(&query(base, &pending, &cursors)?).ok()?;
            let mut repository = response.data.repository;
            let mut next = Vec::new();
            for (index, branch) in pending.into_iter().enumerate() {
                let connection = repository.heads.remove(&format!("h{index}"))?;
                prs.extend(connection.nodes);
                if connection.page_info.has_next_page {
                    let cursor = connection.page_info.end_cursor?;
                    if cursor.is_empty() || cursors.get(branch) == Some(&cursor) {
                        return None;
                    }
                    cursors.insert(branch, cursor);
                    next.push(branch);
                }
            }
            name = Some(repository.name_with_owner);
            pending = next;
        }
    }
    Some((name?, prs))
}

fn best_match<'a>(
    prs: impl IntoIterator<Item = &'a PullRequest>,
    branch: &str,
    repository: &str,
) -> Option<Badge> {
    prs.into_iter()
        .filter(|pr| {
            pr.head_ref_name == branch
                && pr
                    .head_repository
                    .as_ref()
                    .is_some_and(|head| head.name_with_owner.eq_ignore_ascii_case(repository))
        })
        // An open PR takes precedence when a branch name was reused.
        .max_by_key(|pr| (pr.badge.state == Status::Open, pr.badge.number))
        .map(|pr| pr.badge.clone())
}

fn load(repository: &Repository) -> Option<Badges> {
    load_with(repository, |base, branches, cursors| {
        let (owner, name) = owner_and_name(base)?.split_once('/')?;
        let mut command = Command::new("gh");
        command
            .current_dir(repository.directory())
            .args(["api", "graphql", "-f"])
            .arg(format!("query={}", graphql_query(branches.len())))
            .args([
                "-f",
                &format!("owner={owner}"),
                "-f",
                &format!("name={name}"),
            ]);
        for (index, branch) in branches.iter().enumerate() {
            command.args(["-f", &format!("h{index}={branch}")]);
            if let Some(cursor) = cursors.get(branch) {
                command.args(["-f", &format!("c{index}={cursor}")]);
            }
        }
        let output = command
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output.status.success().then_some(output.stdout)
    })
}

/// Badges keyed by local branch. `query` returns the raw GraphQL response for
/// a base repository URL, head branch names, and their continuation cursors.
fn load_with(
    repository: &Repository,
    mut query: impl FnMut(&str, &[&str], &Cursors<'_>) -> Option<Vec<u8>>,
) -> Option<Badges> {
    let remotes = remotes(repository)?;
    let heads = heads(repository, &remotes)?;
    if heads.is_empty() {
        return Some(HashMap::new());
    }
    let branches: Vec<&str> = heads
        .values()
        .map(|head| head.branch.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    // Searching configured base repositories also finds PRs from a fork
    // (origin) into its upstream. Verify the head repository, not just its name.
    let mut canonical = HashMap::new();
    let mut prs = Vec::new();
    for base in remotes
        .values()
        .flat_map(|remote| remote.fetch.iter().chain(remote.push.iter()))
        .collect::<BTreeSet<_>>()
    {
        // An inaccessible remote must not hide another remote's PRs.
        if let Some((name, found)) = base_pull_requests(base, &branches, &mut query) {
            canonical.insert(base.to_ascii_lowercase(), name);
            prs.extend(found);
        }
    }
    if canonical.is_empty() {
        return None;
    }
    let found = heads
        .into_iter()
        .filter_map(|(local, head)| {
            let repository = match canonical.get(&head.repository.to_ascii_lowercase()) {
                Some(name) => name.as_str(),
                None => owner_and_name(&head.repository)?,
            };
            Some((local, best_match(&prs, &head.branch, repository)?))
        })
        .collect();
    Some(found)
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

    /// A GraphQL response from `base`, renamed to `name_with_owner`.
    fn response(name_with_owner: &str, branches: &[&str], prs: &[serde_json::Value]) -> Vec<u8> {
        let mut repository = json!({ "nameWithOwner": name_with_owner });
        for (index, branch) in branches.iter().enumerate() {
            let nodes: Vec<_> = prs
                .iter()
                .filter(|pr| pr["headRefName"] == *branch)
                .collect();
            repository[format!("h{index}")] =
                json!({ "nodes": nodes, "pageInfo": { "hasNextPage": false, "endCursor": null } });
        }
        serde_json::to_vec(&json!({ "data": { "repository": repository } })).unwrap()
    }

    #[test]
    fn pr_connections_follow_all_pages_without_reloading_finished_branches() {
        let mut calls = 0;
        let (_, prs) = base_pull_requests(
            "https://github.com/base/repo",
            &["done", "main"],
            &mut |_, branches, cursors| {
                calls += 1;
                let (nodes, next) = match calls {
                    1 => {
                        assert_eq!(branches, &["done", "main"]);
                        assert!(cursors.is_empty());
                        let mut nodes: Vec<_> = (1..100)
                            .map(|n| pr(n, "other/repo", "main", "OPEN", false))
                            .collect();
                        nodes.push(pr(100, "me/repo", "main", "CLOSED", false));
                        (nodes, Some("first-page"))
                    }
                    2 => {
                        assert_eq!(branches, &["main"]);
                        assert_eq!(cursors.get("main").map(String::as_str), Some("first-page"));
                        let mut nodes: Vec<_> = (101..200)
                            .map(|n| pr(n, "other/repo", "main", "OPEN", false))
                            .collect();
                        nodes.push(pr(200, "me/repo", "main", "MERGED", false));
                        (nodes, Some("second-page"))
                    }
                    3 => {
                        assert_eq!(branches, &["main"]);
                        assert_eq!(cursors.get("main").map(String::as_str), Some("second-page"));
                        (vec![pr(201, "me/repo", "main", "OPEN", true)], None)
                    }
                    _ => panic!("unexpected extra page"),
                };
                let mut page: serde_json::Value =
                    serde_json::from_slice(&response("base/repo", branches, &nodes)).unwrap();
                let index = branches
                    .iter()
                    .position(|branch| *branch == "main")
                    .unwrap();
                page["data"]["repository"][format!("h{index}")]["pageInfo"] = json!({
                    "hasNextPage": next.is_some(), "endCursor": next
                });
                Some(serde_json::to_vec(&page).unwrap())
            },
        )
        .unwrap();
        assert_eq!(
            best_match(&prs, "main", "me/repo").unwrap().label(),
            "#201 draft"
        );
        assert_eq!(calls, 3);
        assert_eq!(prs.len(), 201);
    }

    #[test]
    fn a_failed_pr_page_does_not_publish_partial_badges() {
        let mut calls = 0;
        let result = base_pull_requests(
            "https://github.com/base/repo",
            &["main"],
            &mut |_, branches, _| {
                calls += 1;
                if calls > 1 {
                    return None;
                }
                let mut page: serde_json::Value = serde_json::from_slice(&response(
                    "base/repo",
                    branches,
                    &[pr(1, "me/repo", "main", "CLOSED", false)],
                ))
                .unwrap();
                page["data"]["repository"]["h0"]["pageInfo"] = json!({
                    "hasNextPage": true, "endCursor": "first-page"
                });
                Some(serde_json::to_vec(&page).unwrap())
            },
        );
        assert!(
            result.is_none(),
            "an incomplete response must not look like a complete lookup"
        );
        assert_eq!(calls, 2);
    }

    #[test]
    fn reused_branch_names_prefer_open_prs_and_verify_fork_identity() {
        let prs = vec![
            pr(1, "ME/repo", "topic", "OPEN", true),
            pr(2, "me/repo", "topic", "MERGED", false),
            pr(3, "other/repo", "topic", "OPEN", false),
            pr(4, "me/repo", "different", "OPEN", false),
        ];
        let parse = |prs: &[serde_json::Value]| {
            let prs: Vec<PullRequest> = serde_json::from_value(json!(prs)).unwrap();
            best_match(&prs, "topic", "me/repo")
        };
        assert_eq!(parse(&prs).unwrap().label(), "#1 draft");
        assert_eq!(parse(&prs[1..]).unwrap().label(), "#2 merged");
        let mut deleted = prs[0].clone();
        deleted["headRepository"] = serde_json::Value::Null;
        assert!(parse(&[deleted]).is_none());
    }

    #[test]
    fn push_urls_match_fork_prs_without_losing_fetch_bases() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "topic"]);
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
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/base/repo.git",
        ]);
        git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:old/repo.git",
        ]);
        let mut queried = BTreeSet::new();
        let badges = load_with(&Repository::main().unwrap(), |base, branches, _| {
            queried.insert(base.to_owned());
            Some(if base == "https://github.com/base/repo" {
                response(
                    "base/repo",
                    branches,
                    &[
                        pr(10, "me/repo", "topic", "OPEN", false),
                        pr(20, "base/repo", "topic", "OPEN", false),
                    ],
                )
            } else {
                assert_eq!(base, "https://github.com/old/repo");
                response("me/repo", branches, &[])
            })
        })
        .unwrap();
        assert_eq!(badges["topic"].label(), "#10 open");
        assert_eq!(
            queried,
            BTreeSet::from([
                "https://github.com/base/repo".to_owned(),
                "https://github.com/old/repo".to_owned(),
            ])
        );

        // A push outside GitHub must not inherit the fetch repository's PRs.
        git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            "https://example.invalid/repo.git",
        ]);
        let badges = load_with(&Repository::main().unwrap(), |_, _, _| {
            panic!("a non-GitHub push target must not query the fetch repository")
        })
        .unwrap();
        assert!(badges.is_empty());
    }

    #[test]
    fn renamed_repositories_match_their_canonical_name() {
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
        git(&["remote", "add", "origin", "https://github.com/old/repo.git"]);
        let mut queries = 0;
        let badges = load_with(&Repository::main().unwrap(), |base, branches, _| {
            queries += 1;
            assert_eq!(base, "https://github.com/old/repo");
            Some(response(
                "new/repo",
                branches,
                &[pr(5, "new/repo", "main", "OPEN", false)],
            ))
        })
        .unwrap();
        assert_eq!(queries, 1);
        assert_eq!(badges["main"].label(), "#5 open");
    }

    #[test]
    fn unresolved_upstream_mappings_do_not_attach_origin_pull_requests() {
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
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/base/repo.git",
        ]);
        git(&[
            "remote",
            "add",
            "upstream",
            "https://github.com/me/repo.git",
        ]);
        git(&["config", "branch.main.remote", "upstream"]);
        git(&["config", "branch.main.merge", "refs/heads/topic"]);
        git(&[
            "config",
            "remote.upstream.fetch",
            "+refs/heads/main:refs/remotes/upstream/main",
        ]);
        let badges = load_with(&Repository::main().unwrap(), |_, _, _| {
            panic!("an unresolved upstream must not query origin's same-named branch")
        })
        .unwrap();
        assert!(badges.is_empty());
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
        // Only `upstream` pushes to a differently named upstream branch.
        git(&["config", "push.default", "upstream"]);
        git(&["config", "branch.topic.remote", "origin"]);
        git(&["config", "branch.topic.merge", "refs/heads/topic"]);
        // Tracking refs can be gone after a PR was merged and its branch deleted.
        let repository = Repository::main().unwrap();
        let badges = load_with(&repository, |base, branches, _| {
            if base != "https://github.com/base/repo" {
                return None; // An inaccessible remote must not hide another remote's PRs.
            }
            Some(response(
                "base/repo",
                branches,
                &[pr(10, "me/repo", "topic", "OPEN", false)],
            ))
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
