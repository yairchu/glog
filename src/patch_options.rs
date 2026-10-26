//! Patch display preferences shared by every view in a session.

use std::{process::Command, sync::OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMoved {
    No,
    Default,
    Plain,
    Blocks,
    Zebra,
    DimmedZebra,
}

impl ColorMoved {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Default => "default",
            Self::Plain => "plain",
            Self::Blocks => "blocks",
            Self::Zebra => "zebra",
            Self::DimmedZebra => "dimmed-zebra",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "no" => Ok(Self::No),
            "default" => Ok(Self::Default),
            "plain" => Ok(Self::Plain),
            "blocks" => Ok(Self::Blocks),
            "zebra" => Ok(Self::Zebra),
            "dimmed-zebra" | "dimmed_zebra" => Ok(Self::DimmedZebra),
            _ => Err(format!(
                "invalid --color-moved mode {value:?}; expected no, default, plain, blocks, zebra, or dimmed-zebra"
            )),
        }
    }
}

/// Consume session flags without consuming Git's patterns or literal pathspecs.
pub fn parse_args(args: &[String]) -> Result<(Option<ColorMoved>, Vec<String>), String> {
    let mut mode = None;
    let mut rest = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            rest.push(arg.clone());
            rest.extend(args.cloned());
            break;
        }
        if crate::log_format::takes_pattern(arg)
            || matches!(
                arg.as_str(),
                "--format"
                    | "--pretty"
                    | "--hide-types"
                    | "--date"
                    | "-L"
                    | "-n"
                    | "--max-count"
                    | "--skip"
                    | "--since"
                    | "--until"
                    | "--after"
                    | "--before"
            )
        {
            rest.push(arg.clone());
            if let Some(value) = args.next() {
                rest.push(value.clone());
            }
        } else if arg == "--color-moved" {
            mode = Some(ColorMoved::Zebra);
        } else if arg == "--no-color-moved" {
            mode = Some(ColorMoved::No);
        } else if let Some(value) = arg.strip_prefix("--color-moved=") {
            mode = Some(ColorMoved::parse(value)?);
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((mode, rest))
}

// Fixed before starting background readers; all views share the CLI override.
static SESSION_MODE: OnceLock<Option<ColorMoved>> = OnceLock::new();

pub fn initialize(mode: Option<ColorMoved>) {
    SESSION_MODE
        .set(mode)
        .expect("patch options initialized once");
}

/// Git keeps its own configured mode and whitespace policy. Only supply our
/// default when colorMoved is absent, so explicit false/no remains effective.
pub fn apply(command: &mut Command, directory: Option<&std::path::Path>) {
    apply_mode(command, SESSION_MODE.get().copied().flatten(), directory);
}

fn apply_mode(
    command: &mut Command,
    mode: Option<ColorMoved>,
    directory: Option<&std::path::Path>,
) {
    let mode = mode.or_else(|| {
        let mut config = crate::git::read_command();
        if let Some(directory) = directory {
            crate::git::repository_env(&mut config, directory).current_dir(directory);
        }
        config
            .args(["config", "--get", "diff.colorMoved"])
            .output()
            .ok()
            .filter(|output| output.status.code() == Some(1))
            .map(|_| ColorMoved::Zebra)
    });
    if let Some(mode) = mode {
        command.args(["-c", &format!("diff.colorMoved={}", mode.as_str())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};

    #[test]
    fn session_flags_preserve_patterns_paths_and_other_options() {
        let parse =
            |args: &[&str]| parse_args(&args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>());
        for mode in [
            "no",
            "default",
            "plain",
            "blocks",
            "zebra",
            "dimmed-zebra",
            "dimmed_zebra",
        ] {
            let (selected, rest) = parse(&[&format!("--color-moved={mode}"), "HEAD"]).unwrap();
            assert_eq!(selected, Some(ColorMoved::parse(mode).unwrap()));
            assert_eq!(rest, ["HEAD"]);
        }
        assert_eq!(
            parse(&["--color-moved", "--no-color-moved"]).unwrap().0,
            Some(ColorMoved::No)
        );
        assert_eq!(
            parse(&["--no-color-moved", "--color-moved"]).unwrap().0,
            Some(ColorMoved::Zebra)
        );
        assert!(parse(&["--color-moved="]).is_err());
        assert!(parse(&["--color-moved=unknown"]).is_err());
        for args in [
            vec!["--grep", "--color-moved"],
            vec!["--author", "--no-color-moved"],
            vec!["-S", "--color-moved=zebra"],
            vec!["--format", "--color-moved"],
            vec!["--", "--color-moved", "--no-color-moved"],
        ] {
            let (mode, rest) = parse(&args).unwrap();
            assert_eq!(mode, None);
            assert_eq!(rest, args);
        }
        let (mode, rest) = parse(&["--watch", "--no-color-moved", "--hide-types=test"]).unwrap();
        assert_eq!(mode, Some(ColorMoved::No));
        let (_, _, git_args) = crate::log_format::parse_args(&rest).unwrap();
        assert!(crate::parse_watch(&git_args).unwrap());
    }

    fn patch(mode: Option<ColorMoved>, directory: Option<&std::path::Path>) -> String {
        let mut command = crate::git::read_command();
        if let Some(directory) = directory {
            command.current_dir(directory);
        }
        apply_mode(&mut command, mode, directory);
        let output = command
            .args(["diff", "--color=always", "--no-ext-diff", "--no-textconv"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn moved_colors_preserve_git_and_session_overrides() {
        const CHILD: &str = "GLOG_TEST_MOVED_COLORS_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Check the unset default without inheriting a developer's config.
            let directory = TestDirectory::new();
            let _cwd = CurrentDirGuard::enter(directory.path());
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "patch_options::tests::moved_colors_preserve_git_and_session_overrides",
                ])
                .env(CHILD, "1")
                .env("GIT_CONFIG_GLOBAL", directory.path().join("absent-config"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env_remove("GIT_CONFIG_COUNT")
                .env_remove("GIT_CONFIG_PARAMETERS")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let first = "fn first_function() {\n    alpha_operation();\n    beta_operation();\n}\n";
        let second = "fn second_function() {\n    gamma_operation();\n    delta_operation();\n}\n";
        std::fs::write("file.rs", format!("{first}\n{second}")).unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        std::fs::write("file.rs", format!("{second}\n{first}")).unwrap();
        let zebra = patch(Some(ColorMoved::Zebra), None);
        let plain = patch(Some(ColorMoved::No), None);
        assert_ne!(zebra, plain);
        assert_eq!(crate::ansi::plain(&zebra), crate::ansi::plain(&plain));
        assert_eq!(patch(None, None), zebra);
        for disabled in ["false", "no"] {
            git(&["config", "diff.colorMoved", disabled]);
            assert_eq!(patch(None, None), plain);
            assert_eq!(patch(Some(ColorMoved::Zebra), None), zebra);
        }
        git(&["config", "diff.colorMoved", "blocks"]);
        assert_eq!(patch(None, None), patch(Some(ColorMoved::Blocks), None));
        assert_eq!(patch(Some(ColorMoved::No), None), plain);

        // Git's whitespace policy still controls whether reindented blocks
        // count as moves; the session option changes only their display mode.
        let indent = |text: &str| {
            text.lines()
                .map(|line| format!("    {line}\n"))
                .collect::<String>()
        };
        std::fs::write("file.rs", format!("{}\n{}", indent(second), indent(first))).unwrap();
        git(&["config", "diff.colorMovedWS", "no"]);
        assert_eq!(
            patch(Some(ColorMoved::Zebra), None),
            patch(Some(ColorMoved::No), None)
        );
        git(&["config", "diff.colorMovedWS", "allow-indentation-change"]);
        assert_ne!(
            patch(Some(ColorMoved::Zebra), None),
            patch(Some(ColorMoved::No), None)
        );

        // Querying a child repository must use its own configuration.
        git(&["config", "--unset", "diff.colorMoved"]);
        git(&["init", "-q", "child"]);
        git(&["-C", "child", "config", "diff.colorMoved", "false"]);
        let child = directory.path().join("child");
        let mut command = crate::git::patch_command_at(Some(&child));
        let output = command
            .args(["config", "--get", "diff.colorMoved"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "false");

        // Delta must retain the distinction without changing patch positions.
        let colored = crate::git::format_output(zebra.into_bytes()).unwrap();
        let uncolored = crate::git::format_output(plain.into_bytes()).unwrap();
        assert_ne!(colored, uncolored);
        assert_eq!(crate::ansi::plain(&colored), crate::ansi::plain(&uncolored));
    }
}
