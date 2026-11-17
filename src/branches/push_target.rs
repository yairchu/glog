//! Where `git push` sends each local branch.
//!
//! A branch's upstream is where it pulls from, which is often not where it is
//! pushed: `git checkout -b feature origin/main` tracks `origin/main`, and fork
//! workflows pull from one remote and push to another. Pull requests and the
//! remote copy of a branch's series live at the push target.
use std::collections::{HashMap, HashSet};

use crate::git::{self, Repository};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub remote: String,
    pub branch: String,
    /// The tracking ref, when fetch and push identify the same repository.
    pub tracking: Option<String>,
}

#[derive(Default)]
struct BranchConfig {
    remote: Option<String>,
    push_remote: Option<String>,
    merge: Option<String>,
}

struct Config {
    push_default: String,
    push_remote: Option<String>,
    default_remote: String,
    explicit_push: HashMap<String, Vec<String>>,
    fetch: HashMap<String, Vec<String>>,
    branches: HashMap<String, BranchConfig>,
}

fn read_config(repository: Option<&Repository>) -> Result<Config, String> {
    let output = git::read_command_at(repository)
        .args([
            "config",
            "--null",
            "--get-regexp",
            r"^(branch\..*\.(remote|pushremote|merge)|push\.default|remote\.pushdefault|remote\..*\.(push|fetch))$",
        ])
        .output()
        .map_err(|error| format!("could not read push configuration: {error}"))?;
    // git config returns 1 when no matching keys exist.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(git::stderr_message(
            "could not read push configuration",
            &output.stderr,
        ));
    }
    let remotes = git::read_command_at(repository)
        .arg("remote")
        .output()
        .map_err(|error| format!("could not list remotes: {error}"))?;
    if !remotes.status.success() {
        return Err(git::stderr_message(
            "could not list remotes",
            &remotes.stderr,
        ));
    }
    let remote_names = String::from_utf8_lossy(&remotes.stdout);
    let mut remote_names = remote_names.lines();
    let first = remote_names.next();
    let default_remote = match (first, remote_names.next()) {
        (Some(only), None) => only.to_owned(),
        _ => "origin".into(),
    };
    let mut config = Config {
        push_default: "simple".into(),
        push_remote: None,
        default_remote,
        explicit_push: HashMap::new(),
        fetch: HashMap::new(),
        branches: HashMap::new(),
    };
    for entry in String::from_utf8_lossy(&output.stdout).split_terminator('\0') {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        if key == "push.default" {
            config.push_default = value.to_ascii_lowercase();
            continue;
        }
        if key == "remote.pushdefault" {
            config.push_remote = Some(value.into());
            continue;
        }
        if let Some(remote) = key
            .strip_prefix("remote.")
            .and_then(|key| key.strip_suffix(".push"))
        {
            config
                .explicit_push
                .entry(remote.to_owned())
                .or_default()
                .push(value.into());
            continue;
        }
        if let Some(remote) = key
            .strip_prefix("remote.")
            .and_then(|key| key.strip_suffix(".fetch"))
        {
            config
                .fetch
                .entry(remote.to_owned())
                .or_default()
                .push(value.into());
            continue;
        }
        let Some(key) = key.strip_prefix("branch.") else {
            continue;
        };
        if let Some(name) = key.strip_suffix(".remote") {
            config.branches.entry(name.into()).or_default().remote = Some(value.into());
        } else if let Some(name) = key.strip_suffix(".pushremote") {
            config.branches.entry(name.into()).or_default().push_remote = Some(value.into());
        } else if let Some(name) = key.strip_suffix(".merge") {
            config.branches.entry(name.into()).or_default().merge = Some(value.into());
        }
    }
    Ok(config)
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

/// Map a full remote ref through its fetch refspec, including wildcard renames.
fn tracking_ref(config: &Config, remote: &str, reference: &str) -> Option<String> {
    config.fetch.get(remote)?.iter().find_map(|spec| {
        let (source, destination) = spec.trim_start_matches('+').split_once(':')?;
        if let Some((prefix, suffix)) = source.split_once('*') {
            let matched = reference.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some(destination.replacen('*', matched, 1))
        } else {
            (source == reference).then(|| destination.to_owned())
        }
    })
}

/// Git's push atoms only map fully qualified sources, and do not expand `:`.
/// Resolve shorthand sources with Git so HEAD and ambiguous names obey its rules.
fn fallback_target(
    config: &Config,
    remote: &str,
    name: &str,
    repository: Option<&Repository>,
    sources: &mut HashMap<String, Option<String>>,
    stored_refs: &mut Option<HashSet<String>>,
) -> Option<Target> {
    let local = format!("refs/heads/{name}");
    for spec in config.explicit_push.get(remote)? {
        let spec = spec.strip_prefix('+').unwrap_or(spec);
        if spec == ":" {
            let Some(tracking) = tracking_ref(config, remote, &local) else {
                continue;
            };
            let refs = stored_refs.get_or_insert_with(|| {
                git::read_command_at(repository)
                    .args(["for-each-ref", "--format=%(refname)"])
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
                    .map(|output| {
                        String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            });
            if refs.contains(&tracking) {
                return Some(Target {
                    remote: remote.to_owned(),
                    branch: name.to_owned(),
                    tracking: Some(tracking),
                });
            }
            continue;
        }
        let (source, destination) = spec
            .split_once(':')
            .map_or((spec, None), |(source, destination)| {
                (source, Some(destination))
            });
        if source.is_empty() || source.contains('*') {
            continue;
        }
        let resolved = sources.entry(source.to_owned()).or_insert_with(|| {
            let output = git::read_command_at(repository)
                .args(["rev-parse", "--symbolic-full-name", "--verify", source])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        });
        if resolved.as_deref() != Some(&local) {
            continue;
        }
        // With no destination, Git updates the source branch's full refname.
        let destination = destination.unwrap_or(&local);
        let destination = if destination.starts_with("refs/") {
            destination.to_owned()
        } else {
            format!("refs/heads/{destination}")
        };
        if let Some(branch) = destination.strip_prefix("refs/heads/") {
            return Some(Target {
                remote: remote.to_owned(),
                branch: branch.to_owned(),
                tracking: tracking_ref(config, remote, &destination),
            });
        }
    }
    None
}

/// Fetch refspecs describe the fetch repository, even when Git's push atoms
/// map a different push URL into the same tracking namespace. Only reuse those
/// refs when the resolved transport URLs identify the same repository.
fn fetch_tracks_push(repository: Option<&Repository>, remote: &str) -> bool {
    if remote == "." {
        return true;
    }
    let urls = |push| {
        let mut command = git::read_command_at(repository);
        command.args(["remote", "get-url"]);
        if push {
            command.arg("--push");
        }
        let output = command.args(["--all", remote]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    };
    let (Some(fetch), Some(push)) = (urls(false), urls(true)) else {
        return false;
    };
    let Some(fetch) = fetch.lines().next() else {
        return false;
    };
    let same_repository = |push: &str| {
        fetch == push
            || match (
                git::github_repository_url(fetch),
                git::github_repository_url(push),
            ) {
                (Some(fetch), Some(push)) => fetch.eq_ignore_ascii_case(&push),
                _ => false,
            }
    };
    // Multiple push URLs all receive the update; none may borrow another
    // repository's stored history. Fetching only reads the first fetch URL.
    !push.is_empty() && push.lines().all(same_repository)
}

/// Push targets of local branches, keyed by branch name. Branches that a
/// plain `git push` would refuse to push are omitted rather than guessed.
pub(super) fn load(repository: Option<&Repository>) -> Result<HashMap<String, Target>, String> {
    let config = read_config(repository)?;
    // Under `current`, `%(push)` maps the same-named branch through the push
    // remote's fetch refspecs, or honors explicit push refspecs when present.
    // Git cannot resolve `%(push)` under `simple` in triangular workflows,
    // so the configured mode is applied below instead. `push:remoteref`
    // supplies explicit destinations even without a matching fetch refspec.
    let output = git::read_command_at(repository)
        .args([
            "-c",
            "push.default=current",
            "for-each-ref",
            "--format=%(refname:strip=2)%00%(push)%00%(upstream)%00%(push:remoteref)",
            "refs/heads/",
        ])
        .output()
        .map_err(|error| format!("could not read push targets: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message(
            "could not read push targets",
            &output.stderr,
        ));
    }
    let mut targets = HashMap::new();
    let mut sources = HashMap::new();
    let mut stored_refs = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let [name, push_tracking, upstream_tracking, push_ref] =
            line.split('\0').collect::<Vec<_>>()[..]
        else {
            return Err("invalid Git push target output".into());
        };
        let branch = config.branches.get(name);
        let fetch_remote = branch
            .and_then(|branch| branch.remote.as_deref())
            .unwrap_or(&config.default_remote);
        // The precedence `git push` uses to choose a remote.
        let remote = branch
            .and_then(|branch| branch.push_remote.as_deref())
            .or(config.push_remote.as_deref())
            .unwrap_or(fetch_remote);
        // Explicit refspecs take precedence over push.default. Let Git map
        // force prefixes and wildcards, and omit branches they do not push.
        if config.explicit_push.contains_key(remote) {
            let target = push_ref
                .strip_prefix("refs/heads/")
                .map(|destination| Target {
                    remote: remote.to_owned(),
                    branch: destination.to_owned(),
                    tracking: non_empty(push_tracking),
                })
                .or_else(|| {
                    fallback_target(
                        &config,
                        remote,
                        name,
                        repository,
                        &mut sources,
                        &mut stored_refs,
                    )
                });
            if let Some(target) = target {
                targets.insert(name.to_owned(), target);
            }
            continue;
        }
        let upstream = branch
            .and_then(|branch| branch.merge.as_deref())
            .map(|merge| merge.strip_prefix("refs/heads/"));
        let triangular = remote != fetch_remote;
        let same_name = Target {
            remote: remote.to_owned(),
            branch: name.to_owned(),
            tracking: non_empty(push_tracking),
        };
        let target = match config.push_default.as_str() {
            "nothing" => None,
            "current" | "matching" => Some(same_name),
            // `tracking` is the deprecated name for `upstream`.
            "upstream" | "tracking" => match upstream {
                Some(Some(upstream)) if !triangular => Some(Target {
                    remote: remote.to_owned(),
                    branch: upstream.to_owned(),
                    tracking: non_empty(upstream_tracking),
                }),
                _ => None,
            },
            // `simple` refuses an upstream with a different name, such as
            // `origin/main` for a branch created from it. Without an
            // upstream, a branch is pushed by naming the remote.
            _ => (triangular || upstream.is_none_or(|upstream| upstream == Some(name)))
                .then_some(same_name),
        };
        if let Some(target) = target {
            targets.insert(name.to_owned(), target);
        }
    }
    let mut identities = HashMap::new();
    for target in targets
        .values_mut()
        .filter(|target| target.tracking.is_some())
    {
        let same_repository = identities
            .entry(target.remote.clone())
            .or_insert_with(|| fetch_tracks_push(repository, &target.remote));
        if !*same_repository {
            target.tracking = None;
        }
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};

    fn target(remote: &str, branch: &str, tracking: Option<&str>) -> Option<Target> {
        Some(Target {
            remote: remote.into(),
            branch: branch.into(),
            tracking: tracking.map(Into::into),
        })
    }

    fn init(remote: &str) {
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
        git(&["remote", "add", remote, "https://example.invalid/repo.git"]);
    }

    #[test]
    fn an_unconfigured_branch_uses_the_only_remote_even_when_it_is_not_origin() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init("fork");
        git(&["config", "push.default", "current"]);
        assert_eq!(
            load(None).unwrap().get("main").cloned(),
            target("fork", "main", Some("refs/remotes/fork/main"))
        );
    }

    #[test]
    fn shorthand_push_refspecs_resolve_branch_destinations() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init_push_remote();
        for spec in [
            "main:published",
            "+main:published",
            "main:refs/heads/published",
        ] {
            git(&["config", "remote.origin.push", spec]);
            assert!(git(&["push", "--dry-run", "--porcelain"])
                .contains("refs/heads/main:refs/heads/published"));
            let targets = load(None).unwrap();
            assert_eq!(
                targets.get("main").cloned(),
                target("origin", "published", Some("refs/remotes/origin/published")),
                "refspec={spec}"
            );
            assert_eq!(targets.get("excluded"), None);
        }
        // Fetch mappings may rename or exclude the configured destination.
        git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/review/*",
        ]);
        assert_eq!(
            load(None).unwrap().get("main").cloned(),
            target("origin", "published", Some("refs/remotes/review/published"))
        );
        git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/main:refs/remotes/origin/main",
        ]);
        assert_eq!(
            load(None).unwrap().get("main").cloned(),
            target("origin", "published", None)
        );
    }

    #[test]
    fn head_push_refspecs_follow_the_checked_out_branch() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init_push_remote();
        git(&["config", "remote.origin.push", "HEAD:refs/heads/published"]);
        for branch in ["main", "excluded"] {
            git(&["checkout", "-q", branch]);
            assert!(
                git(&["push", "--dry-run", "--porcelain"]).contains("HEAD:refs/heads/published")
            );
            let targets = load(None).unwrap();
            assert_eq!(targets.len(), 1);
            assert_eq!(
                targets.get(branch).cloned(),
                target("origin", "published", Some("refs/remotes/origin/published"))
            );
        }
    }

    #[test]
    fn matching_push_refspecs_use_stored_remote_branches() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init_push_remote();
        for spec in [":", "+:"] {
            git(&["config", "remote.origin.push", spec]);
            assert!(git(&["push", "--dry-run", "--porcelain"])
                .contains("refs/heads/main:refs/heads/main"));
            let targets = load(None).unwrap();
            assert_eq!(
                targets.get("main").cloned(),
                target("origin", "main", Some("refs/remotes/origin/main")),
                "refspec={spec}"
            );
            assert_eq!(targets.get("excluded"), None);
        }
    }

    fn init_push_remote() {
        init("origin");
        git(&["init", "--bare", "-q", "remote.git"]);
        git(&["remote", "set-url", "origin", "remote.git"]);
        git(&["push", "-q", "origin", "main", "main:published"]);
        git(&["branch", "excluded"]);
    }

    #[test]
    fn explicit_push_refspecs_override_default_modes_and_resolve_destination_names() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init("origin");
        git(&["branch", "excluded"]);
        git(&["config", "branch.main.remote", "origin"]);
        git(&["config", "branch.main.merge", "refs/heads/base"]);
        git(&[
            "config",
            "remote.origin.push",
            "+refs/heads/main:refs/heads/published",
        ]);
        for mode in ["current", "simple", "upstream", "nothing"] {
            git(&["config", "push.default", mode]);
            let targets = load(None).unwrap();
            assert_eq!(
                targets.get("main").cloned(),
                target("origin", "published", Some("refs/remotes/origin/published")),
                "push.default={mode}"
            );
            assert_eq!(targets.get("excluded"), None);
        }
        // A destination can still identify PRs without a matching fetch ref.
        git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/base:refs/remotes/origin/base",
        ]);
        assert_eq!(
            load(None).unwrap().get("main").cloned(),
            target("origin", "published", None)
        );
        git(&[
            "config",
            "remote.origin.push",
            "refs/heads/*:refs/heads/review/*",
        ]);
        assert_eq!(
            load(None).unwrap().get("main").cloned(),
            target("origin", "review/main", None)
        );
    }

    #[test]
    fn branches_created_from_another_remote_branch_push_to_their_own_name() {
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
            "https://example.invalid/repo.git",
        ]);
        git(&["remote", "add", "fork", "https://example.invalid/fork.git"]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&["branch", "--track", "feature", "origin/main"]);
        git(&["branch", "--track", "main-copy", "main"]);
        let load = || load(None).unwrap();

        // `simple` refuses to push feature to its upstream, origin/main.
        let targets = load();
        assert_eq!(targets.get("feature"), None);
        assert_eq!(
            targets.get("main").cloned(),
            target("origin", "main", Some("refs/remotes/origin/main"))
        );
        // A local upstream is not a push target.
        assert_eq!(targets.get("main-copy"), None);

        // Fork workflows pull from origin and push the same name elsewhere.
        git(&["config", "remote.pushDefault", "fork"]);
        assert_eq!(
            load().get("feature").cloned(),
            target("fork", "feature", Some("refs/remotes/fork/feature"))
        );

        git(&["config", "--unset", "remote.pushDefault"]);
        for mode in ["upstream", "tracking"] {
            git(&["config", "push.default", mode]);
            assert_eq!(
                load().get("feature").cloned(),
                target("origin", "main", Some("refs/remotes/origin/main"))
            );
        }
        git(&["config", "push.default", "current"]);
        assert_eq!(
            load().get("feature").cloned(),
            target("origin", "feature", Some("refs/remotes/origin/feature"))
        );
    }
}
