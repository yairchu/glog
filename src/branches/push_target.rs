//! Where `git push` sends each local branch.
//!
//! A branch's upstream is where it pulls from, which is often not where it is
//! pushed: `git checkout -b feature origin/main` tracks `origin/main`, and fork
//! workflows pull from one remote and push to another. Pull requests and the
//! remote copy of a branch's series live at the push target.
use std::collections::HashMap;

use crate::git::{self, Repository};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub remote: String,
    pub branch: String,
    /// The remote-tracking ref, unless the remote's fetch mapping excludes it.
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
    branches: HashMap<String, BranchConfig>,
}

fn read_config(repository: Option<&Repository>) -> Result<Config, String> {
    let output = git::read_command_at(repository)
        .args([
            "config",
            "--null",
            "--get-regexp",
            r"^(branch\..*\.(remote|pushremote|merge)|push\.default|remote\.pushdefault)$",
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
    let mut config = Config {
        push_default: "simple".into(),
        push_remote: None,
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

/// Push targets of local branches, keyed by branch name. Branches that a
/// plain `git push` would refuse to push are omitted rather than guessed.
pub(super) fn load(repository: Option<&Repository>) -> Result<HashMap<String, Target>, String> {
    let config = read_config(repository)?;
    // Under `current`, `%(push)` maps the same-named branch through the push
    // remote's fetch refspecs. Git cannot resolve `%(push)` under `simple` in
    // triangular workflows, so the configured mode is applied below instead.
    let output = git::read_command_at(repository)
        .args([
            "-c",
            "push.default=current",
            "for-each-ref",
            "--format=%(refname:strip=2)%00%(push)%00%(upstream)",
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
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let [name, same_name_tracking, upstream_tracking] =
            line.split('\0').collect::<Vec<_>>()[..]
        else {
            return Err("invalid Git push target output".into());
        };
        let branch = config.branches.get(name);
        let fetch_remote = branch
            .and_then(|branch| branch.remote.as_deref())
            .unwrap_or("origin");
        // The precedence `git push` uses to choose a remote.
        let remote = branch
            .and_then(|branch| branch.push_remote.as_deref())
            .or(config.push_remote.as_deref())
            .unwrap_or(fetch_remote);
        let upstream = branch
            .and_then(|branch| branch.merge.as_deref())
            .map(|merge| merge.strip_prefix("refs/heads/"));
        let triangular = remote != fetch_remote;
        let same_name = Target {
            remote: remote.to_owned(),
            branch: name.to_owned(),
            tracking: non_empty(same_name_tracking),
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
