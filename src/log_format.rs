use ratatui::{
    style::{Color, Style},
    text::Span,
};

use crate::git::{Annotation, Collaborators, Commit, CommitKind, ReflogEntry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Hash,
    Date,
    Author,
    Refs,
    Subject,
}

#[derive(Clone, Debug)]
struct Part {
    field: Field,
    token: String,
    prefix: String,
    suffix: String,
    enabled: bool,
}

#[derive(Clone, Debug)]
pub struct LogFormat {
    parts: Vec<Part>,
}

impl Default for LogFormat {
    fn default() -> Self {
        Self::parse("%h %ad %an (%D) %s").unwrap()
    }
}

impl LogFormat {
    /// Literals precede the next field; matching closing brackets/quotes belong
    /// to the preceding field. The final literal belongs to the final field.
    pub fn parse(source: &str) -> Result<Self, String> {
        if source.chars().any(char::is_control) {
            return Err("log formats must fit on one line".into());
        }
        let mut parts: Vec<Part> = Vec::new();
        let mut literal = String::new();
        let mut rest = source;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("%%") {
                literal.push('%');
                rest = after;
            } else if let Some(after) = rest.strip_prefix('%') {
                let (token, field) = [
                    ("ad", Field::Date), ("an", Field::Author), ("ae", Field::Author),
                    ("h", Field::Hash), ("H", Field::Hash), ("d", Field::Refs),
                    ("D", Field::Refs), ("s", Field::Subject),
                ].into_iter().find(|(token, _)| after.starts_with(token))
                    .ok_or_else(|| format!("unsupported log format near %{after}; supported: %h %H %ad %an %ae %d %D %s %%"))?;
                parts.push(Part {
                    field,
                    token: token.into(),
                    prefix: std::mem::take(&mut literal),
                    suffix: String::new(),
                    enabled: true,
                });
                rest = &after[token.len()..];
                // Closing punctuation belongs to the field it follows, including
                // wrappers with labels, e.g. " (author: %an)".
                let part = parts.last_mut().unwrap();
                while let Some(ch) = rest.chars().next() {
                    if ")]}>.,;:!?".contains(ch)
                        || ((ch == '\'' || ch == '"') && part.prefix.contains(ch))
                    {
                        part.suffix.push(ch);
                        rest = &rest[ch.len_utf8()..];
                    } else {
                        break;
                    }
                }
            } else {
                let ch = rest.chars().next().unwrap();
                literal.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
        if let Some(last) = parts.last_mut() {
            last.suffix.push_str(&literal);
        } else {
            return Err("log format must contain at least one supported field".into());
        }
        Ok(Self { parts })
    }

    pub fn shows(&self, field: Field) -> bool {
        self.parts
            .iter()
            .any(|part| part.field == field && part.enabled)
    }

    pub fn toggle(&mut self, field: Field) {
        let enabled = !self.shows(field);
        if !self.parts.iter().any(|part| part.field == field) {
            let source = match field {
                Field::Hash => " %h",
                Field::Date => " %ad",
                Field::Author => " %an",
                Field::Refs => " (%D)",
                Field::Subject => " %s",
            };
            let part = Self::parse(source).unwrap().parts.remove(0);
            let index = self
                .parts
                .iter()
                .position(|part| part.field == Field::Subject)
                .unwrap_or(self.parts.len());
            if let Some(subject) = self.parts.get_mut(index) {
                if subject.prefix.is_empty() {
                    subject.prefix.push(' ');
                }
            }
            self.parts.insert(index, part);
        }
        for part in &mut self.parts {
            if part.field == field {
                part.enabled = enabled;
            }
        }
    }

    pub fn spans(&self, commit: &Commit) -> Vec<Span<'static>> {
        match &commit.annotation {
            Some(Annotation::Reflog(entry)) => return reflog_spans(commit, entry),
            Some(Annotation::Stash(entry)) => return crate::stash::spans(commit, entry, self),
            Some(Annotation::Blame(line)) => {
                return crate::blame::spans(commit, line, line.starts_chunk, self)
            }
            None => {}
        }
        // Synthetic entries must remain identifiable even in author-only formats.
        if commit.kind != CommitKind::Revision {
            return vec![
                Span::styled(
                    format!("{} ", commit.short_hash),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(commit.subject.clone()),
            ];
        }
        let mut spans = Vec::new();
        // Append badges once, after the last visible author name/email field.
        let last_author = self.parts.iter().rposition(|part| {
            part.enabled
                && match part.token.as_str() {
                    "an" => !commit.author.is_empty(),
                    "ae" => !commit.author_email.is_empty(),
                    _ => false,
                }
        });
        for (index, part) in self
            .parts
            .iter()
            .enumerate()
            .filter(|(_, part)| part.enabled)
        {
            let value = match part.token.as_str() {
                "h" => &commit.short_hash,
                "H" => &commit.hash,
                "ad" => &commit.author_date,
                "an" => &commit.author,
                "ae" => &commit.author_email,
                "d" | "D" => &commit.decorations,
                _ => &commit.subject,
            };
            if value.is_empty() {
                continue;
            }
            let value = if part.token == "d" {
                format!(" ({value})")
            } else {
                value.clone()
            };
            let text = format!("{}{value}{}", part.prefix, part.suffix);
            let text = if spans.is_empty() {
                text.trim_start().to_owned()
            } else {
                text
            };
            let style = match part.field {
                Field::Hash => Style::default().fg(Color::Yellow),
                Field::Date => Style::default().fg(Color::Gray),
                Field::Author => Style::default().fg(Color::Cyan),
                Field::Refs => Style::default().fg(Color::Green),
                Field::Subject => Style::default(),
            };
            if let Some(kind) = (part.field == Field::Subject)
                .then(|| crate::log_folds::commit_type_prefix(commit))
                .flatten()
            {
                // Preserve format literals and the subject's original spelling.
                let start = text.len() - value.len() - part.suffix.len();
                if start > 0 {
                    spans.push(Span::styled(text[..start].to_owned(), style));
                }
                spans.push(Span::styled(kind.to_owned(), style.fg(Color::LightBlue)));
                spans.push(Span::styled(text[start + kind.len()..].to_owned(), style));
            } else {
                spans.push(Span::styled(text, style));
            }
            if Some(index) == last_author {
                spans.extend(coauthor_badge(&commit.collaborators));
            }
        }
        spans
    }

    pub fn text(&self, commit: &Commit) -> String {
        self.spans(commit)
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}

/// A `+` and a symbol for an agent coauthor, or the number of coauthors.
pub fn coauthor_badge(collaborators: &Collaborators) -> Vec<Span<'static>> {
    let total =
        usize::from(collaborators.codex) + usize::from(collaborators.claude) + collaborators.others;
    if total == 0 {
        return Vec::new();
    }
    let badge = if total == 1 && collaborators.codex {
        Span::raw("꩜")
    } else if total == 1 && collaborators.claude {
        Span::styled("❋", Style::default().fg(Color::Rgb(215, 119, 87)))
    } else {
        Span::styled(total.to_string(), Style::default().fg(Color::Gray))
    };
    vec![
        Span::styled("+", Style::default().fg(Color::Rgb(160, 160, 160))),
        badge,
    ]
}

fn reflog_spans(commit: &Commit, entry: &ReflogEntry) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(
            format!("{} ", commit.short_hash),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("{} ", entry.updated_at),
            Style::default().fg(Color::Gray),
        ),
        Span::styled(
            format!("{} ", entry.actor),
            Style::default().fg(Color::Cyan),
        ),
    ];
    if entry.show_reference {
        spans.push(Span::styled(
            format!("({}) ", entry.reference),
            Style::default().fg(Color::Green),
        ));
    }
    if let Some((action, message)) = entry.action.split_once(": ") {
        spans.push(Span::styled(
            format!("{action}:"),
            Style::default().fg(Color::Rgb(255, 165, 0)),
        ));
        spans.push(Span::raw(" "));
        if let Some(kind) = crate::log_folds::subject_type_prefix(message) {
            spans.push(Span::styled(
                kind.to_owned(),
                Style::default().fg(Color::LightBlue),
            ));
            spans.push(Span::raw(message[kind.len()..].to_owned()));
        } else {
            spans.push(Span::raw(message.to_owned()));
        }
    } else {
        spans.push(Span::raw(entry.action.clone()));
    }
    spans
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogOptions {
    pub fold_merges: bool,
    pub hide_merges: bool,
    pub hidden_types: std::collections::BTreeSet<String>,
}

/// Consume display options before `--`, leaving traversal and pathspecs for Git.
pub fn parse_args(args: &[String]) -> Result<(LogFormat, LogOptions, Vec<String>), String> {
    let mut format = LogFormat::default();
    let mut git_args = Vec::new();
    let mut options = LogOptions::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        // Git consumes the next argument as a pattern even when it looks like
        // a display flag or the pathspec separator.
        if matches!(
            arg.as_str(),
            "--grep" | "--grep-reflog" | "--author" | "--committer" | "-G" | "-S"
        ) {
            git_args.push(arg.clone());
            git_args.push(
                args.next()
                    .ok_or_else(|| format!("{arg} requires a pattern"))?
                    .clone(),
            );
            continue;
        }
        if arg == "--" {
            git_args.push(arg.clone());
            git_args.extend(args.cloned());
            break;
        }
        if arg == "--hide-types" || arg.starts_with("--hide-types=") {
            let value = if arg == "--hide-types" {
                args.next()
                    .ok_or("--hide-types requires a comma-separated list of types")?
                    .as_str()
            } else {
                arg.strip_prefix("--hide-types=").unwrap()
            };
            for kind in value.split(',') {
                let kind = crate::log_folds::normalize_type(kind.trim())
                    .ok_or_else(|| format!("invalid commit type {kind:?} in --hide-types"))?;
                options.hidden_types.insert(kind.to_owned());
            }
            continue;
        }
        let value = if arg == "--format" || arg == "--pretty" {
            Some(
                args.next()
                    .ok_or_else(|| format!("{arg} requires a format"))?
                    .as_str(),
            )
        } else {
            arg.strip_prefix("--format=")
                .or_else(|| arg.strip_prefix("--pretty="))
        };
        if let Some(value) = value {
            let value = value
                .strip_prefix("format:")
                .or_else(|| value.strip_prefix("tformat:"))
                .unwrap_or(value);
            format = if value == "oneline" {
                LogFormat::parse("%h (%D) %s")?
            } else {
                LogFormat::parse(value)?
            };
        } else if arg == "--fold-merges" {
            options.fold_merges = true;
        } else if arg == "--hide-merges" {
            options.hide_merges = true;
        } else if arg == "--oneline" {
            format = LogFormat::parse("%h (%D) %s")?;
        } else if arg == "--no-graph" {
            return Err("--no-graph is not supported; glog always draws the graph".into());
        } else {
            git_args.push(arg.clone());
        }
    }
    Ok((format, options, git_args))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn reflog_actions_and_subject_types_have_distinct_colors() {
        for (action, label, kind) in [
            ("commit: feat(log)!: café", Some("commit:"), Some("feat")),
            (
                "commit (amend): tests #234(scope): coverage",
                Some("commit (amend):"),
                Some("tests"),
            ),
            (
                "rebase (pick): doc: explain",
                Some("rebase (pick):"),
                Some("doc"),
            ),
            (
                "checkout: moving from main to topic",
                Some("checkout:"),
                None,
            ),
            ("reset: moving to HEAD~1", Some("reset:"), None),
            ("custom message", None, None),
        ] {
            let mut c = commit();
            c.annotation = Some(crate::git::Annotation::Reflog(crate::git::ReflogEntry {
                reference: "HEAD".into(),
                show_reference: false,
                updated_at: "2026-10-01 10:00:01".into(),
                actor: "Alice".into(),
                action: action.into(),
            }));
            let format = LogFormat::default();
            assert_eq!(
                format.text(&c),
                format!("{} 2026-10-01 10:00:01 Alice {action}", c.short_hash)
            );
            let spans = format.spans(&c);
            for (color, expected) in [(Color::Rgb(255, 165, 0), label), (Color::LightBlue, kind)] {
                let parts: Vec<_> = spans
                    .iter()
                    .filter(|span| span.style.fg == Some(color))
                    .map(|span| span.content.as_ref())
                    .collect();
                assert_eq!(parts, expected.into_iter().collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn subject_types_keep_original_spelling_and_format_literals() {
        for (subject, kind) in [
            ("feat(log)!: add filtering", Some("feat")),
            ("tests #234(failing): reproduce crash", Some("tests")),
            ("doc: explain filtering", Some("doc")),
            ("feature: new view", Some("feature")),
            ("custom-type: café", Some("custom-type")),
            ("ordinary prose: description", None),
            ("fix(): invalid scope", None),
        ] {
            let mut c = commit();
            c.subject = subject.into();
            for source in ["%s", "  «%s»", "%h — %s", "%s / %s"] {
                let format = LogFormat::parse(source).unwrap();
                let expected = source.replace("%s", subject).replace("%h", &c.short_hash);
                assert_eq!(format.text(&c), expected.trim_start());
                let spans = format.spans(&c);
                let highlighted: Vec<_> = spans
                    .iter()
                    .filter(|span| span.style.fg == Some(Color::LightBlue))
                    .map(|span| span.content.as_ref())
                    .collect();
                let expected = kind
                    .map(|kind| vec![kind; source.matches("%s").count()])
                    .unwrap_or_default();
                assert_eq!(highlighted, expected);
            }
            let mut format = LogFormat::parse("%s").unwrap();
            format.toggle(Field::Subject);
            assert!(format.spans(&c).is_empty());
            c.kind = CommitKind::WorkingTree;
            assert!(LogFormat::default()
                .spans(&c)
                .iter()
                .all(|span| { span.style.fg != Some(Color::LightBlue) }));
        }
    }

    #[test]
    fn startup_filters_parse_aliases_repetitions_and_preserve_git_arguments() {
        let args = [
            "--hide-types=tests,doc",
            "--hide-types",
            "feature,test,custom",
            "--hide-merges",
            "--fold-merges",
            "--all",
        ]
        .map(str::to_owned);
        let (_, options, git) = parse_args(&args).unwrap();
        assert!(options.hide_merges && options.fold_merges);
        assert_eq!(
            options.hidden_types,
            ["test", "docs", "feat", "custom"].map(str::to_owned).into()
        );
        assert_eq!(git, ["--all"]);
        for args in [
            vec!["--hide-types"],
            vec!["--hide-types="],
            vec!["--hide-types=test,"],
            vec!["--hide-types=fix: bad"],
            vec!["--hide-types", "--hide-merges"],
        ] {
            assert!(parse_args(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
        }
        for args in [
            vec!["--grep", "--hide-merges"],
            vec!["--author", "--hide-types=test"],
            vec!["--", "--hide-types=test", "--hide-merges"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
            let (_, options, git) = parse_args(&args).unwrap();
            assert_eq!(options, LogOptions::default());
            assert_eq!(git, args);
        }
    }

    #[test]
    fn fold_merges_is_a_display_option_not_a_pattern_or_path() {
        for args in [
            vec!["--fold-merges", "--oneline", "--all"],
            vec!["--format=%h %s", "--fold-merges", "--all"],
            vec!["--fold-merges", "--format=%h %s", "--all"],
        ] {
            let (_, fold_merges, git) =
                parse_args(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).unwrap();
            assert!(fold_merges.fold_merges);
            assert_eq!(git, ["--all"]);
        }
        for args in [
            vec!["--grep", "--fold-merges"],
            vec!["--", "--fold-merges"],
            vec!["--format", "--fold-merges %s"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
            let (_, fold_merges, git) = parse_args(&args).unwrap();
            assert!(!fold_merges.fold_merges);
            if args[0] != "--format" {
                assert_eq!(git, args);
            }
        }
    }

    pub fn commit() -> Commit {
        Commit {
            kind: CommitKind::Revision,
            annotation: None,
            parents: Vec::new(),
            diff_args: Vec::new(),
            hash: "abcdef0123456789".into(),
            short_hash: "abcdef0".into(),
            decorations: "HEAD -> main".into(),
            subject: "A subject".into(),
            author: "Alice".into(),
            author_email: "alice@example.com".into(),
            author_date: "2026-09-14".into(),
            collaborators: crate::git::Collaborators::default(),
            graph: vec!["* ".into()],
        }
    }

    #[test]
    fn only_single_recognized_coauthors_use_icons() {
        for (codex, claude, others, expected) in [
            (false, false, 0, "Alice"),
            (true, false, 0, "Alice+꩜"),
            (false, true, 0, "Alice+❋"),
            (false, false, 1, "Alice+1"),
            (true, true, 0, "Alice+2"),
            (false, true, 1, "Alice+2"),
            (true, false, 1, "Alice+2"),
            (false, false, 2, "Alice+2"),
            (true, true, 2, "Alice+4"),
        ] {
            let mut c = commit();
            c.collaborators = crate::git::Collaborators {
                codex,
                claude,
                others,
            };
            assert_eq!(LogFormat::parse("%an").unwrap().text(&c), expected);
        }
    }

    #[test]
    fn collaborator_badges_follow_author_once_and_toggle_with_it() {
        let mut c = commit();
        c.collaborators = crate::git::Collaborators {
            codex: true,
            claude: true,
            others: 2,
        };
        for (source, expected) in [
            ("%h (%an) %s", "abcdef0 (Alice)+4 A subject"),
            (
                "%h (%an <%ae>) %s",
                "abcdef0 (Alice <alice@example.com>)+4 A subject",
            ),
            ("%h <%ae> %s", "abcdef0 <alice@example.com>+4 A subject"),
        ] {
            let mut format = LogFormat::parse(source).unwrap();
            assert_eq!(format.text(&c), expected);
            format.toggle(Field::Author);
            assert_eq!(format.text(&c), "abcdef0 A subject");
            format.toggle(Field::Author);
            assert_eq!(format.text(&c), expected);
        }
        assert_eq!(
            LogFormat::default().text(&c),
            "abcdef0 2026-09-14 Alice+4 (HEAD -> main) A subject"
        );
        c.collaborators = crate::git::Collaborators {
            others: 1,
            ..Default::default()
        };
        assert_eq!(LogFormat::parse("%an").unwrap().text(&c), "Alice+1");
    }

    #[test]
    fn toggles_remove_and_restore_wrappers_labels_and_repeated_fields() {
        for source in [
            "%h (%an) %s",
            "%h (author: %an), %s",
            "%h [%an] %s",
            "%h \"%an\" %s",
            "%h (%an <%ae>) %s",
        ] {
            let mut format = LogFormat::parse(source).unwrap();
            let original = format.text(&commit());
            format.toggle(Field::Author);
            assert_eq!(format.text(&commit()), "abcdef0 A subject", "{source}");
            format.toggle(Field::Author);
            assert_eq!(format.text(&commit()), original);
        }
    }

    #[test]
    fn default_layout_and_new_fields_have_clean_spacing() {
        let mut format = LogFormat::default();
        assert_eq!(
            format.text(&commit()),
            "abcdef0 2026-09-14 Alice (HEAD -> main) A subject"
        );
        format.toggle(Field::Author);
        format.toggle(Field::Date);
        assert_eq!(format.text(&commit()), "abcdef0 (HEAD -> main) A subject");
        let (compact, _, _) = parse_args(&["--oneline".into()]).unwrap();
        assert_eq!(compact.text(&commit()), format.text(&commit()));
        let mut format = LogFormat::parse("%s").unwrap();
        format.toggle(Field::Author);
        format.toggle(Field::Date);
        assert_eq!(format.text(&commit()), "Alice 2026-09-14 A subject");
        format.toggle(Field::Author);
        format.toggle(Field::Date);
        assert_eq!(format.text(&commit()), "A subject");
    }

    #[test]
    fn renders_git_fields_and_omits_empty_decorations() {
        let mut c = commit();
        let format = LogFormat::parse("%H %ad %an <%ae>%d %s %%").unwrap();
        assert_eq!(
            format.text(&c),
            "abcdef0123456789 2026-09-14 Alice <alice@example.com> (HEAD -> main) A subject %"
        );
        c.decorations.clear();
        assert_eq!(
            LogFormat::default().text(&c),
            "abcdef0 2026-09-14 Alice A subject"
        );
        c.kind = CommitKind::Unstaged;
        assert_eq!(
            LogFormat::parse("%an").unwrap().text(&c),
            "abcdef0 A subject"
        );
    }

    #[test]
    fn git_filter_values_are_not_parsed_as_display_options() {
        for option in [
            "--grep",
            "--grep-reflog",
            "--author",
            "--committer",
            "-G",
            "-S",
        ] {
            for pattern in ["--oneline", "--format=%s", "--pretty", "--"] {
                let args = [option, pattern, "--format=%h"].map(str::to_owned);
                let (format, _, forwarded) = parse_args(&args).unwrap();
                assert_eq!(forwarded, [option, pattern], "{args:?}");
                assert_eq!(format.text(&commit()), "abcdef0", "{args:?}");
            }
            assert!(parse_args(&[option.into()]).is_err());
        }
    }

    #[test]
    fn parses_options_without_consuming_pathspecs_and_last_format_wins() {
        let args = [
            "--pretty=format:%h %ad %an %s",
            "--date=short",
            "main",
            "--",
            "--format=%b",
        ]
        .map(str::to_owned);
        let (format, _, args) = parse_args(&args).unwrap();
        assert_eq!(format.text(&commit()), "abcdef0 2026-09-14 Alice A subject");
        assert_eq!(args, ["--date=short", "main", "--", "--format=%b"]);
        let args = ["--oneline", "--format", "%s"].map(str::to_owned);
        assert_eq!(parse_args(&args).unwrap().0.text(&commit()), "A subject");
        for source in ["%b", "%n", "%C(red)%s", "%", "literal", "%s\n"] {
            assert!(LogFormat::parse(source).is_err(), "{source}");
        }
        assert!(parse_args(&["--pretty".into()]).is_err());
    }
}
