//! The repositories glog reads, resolved once so their kind travels with them.
use std::path::{Path, PathBuf};

use super::{raw_path, read_command, stderr_message};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Repository {
    /// Where Git runs: the working tree root, or a bare repository's Git
    /// directory.
    directory: PathBuf,
    bare: bool,
}

impl Repository {
    /// The repository glog started in.
    pub(crate) fn main() -> Result<Self, String> {
        let output = read_command()
            .args(["rev-parse", "--is-bare-repository"])
            .output()
            .map_err(|error| format!("could not run git rev-parse: {error}"))?;
        if !output.status.success() {
            return Err(stderr_message(
                "could not inspect the repository",
                &output.stderr,
            ));
        }
        let bare = output.stdout.starts_with(b"true");
        let output = read_command()
            .args([
                "rev-parse",
                if bare {
                    "--absolute-git-dir"
                } else {
                    "--show-toplevel"
                },
            ])
            .output()
            .map_err(|error| format!("could not run git rev-parse: {error}"))?;
        if !output.status.success() {
            return Err(stderr_message(
                "could not find the repository root",
                &output.stderr,
            ));
        }
        // Git terminates its raw repository path with exactly one newline.
        let directory = raw_path(output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout));
        Ok(Self { directory, bare })
    }

    /// The repository checked out at `root`, such as a submodule.
    pub(crate) fn checked_out(root: PathBuf) -> Self {
        Self {
            directory: root,
            bare: false,
        }
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    /// Where the repository's files are checked out.
    pub(crate) fn work_tree(&self) -> Result<&Path, String> {
        if self.bare {
            return Err("this operation must be run in a work tree".into());
        }
        Ok(&self.directory)
    }
}
