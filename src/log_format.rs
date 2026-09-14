use ratatui::{
    style::{Color, Style},
    text::Span,
};

use crate::git::{Commit, CommitKind};

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

    pub fn toggle(&mut self, field: Field) {
        let enabled = !self
            .parts
            .iter()
            .any(|part| part.field == field && part.enabled);
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
            spans.push(Span::styled(text, style));
            if Some(index) == last_author {
                let collaborators = &commit.collaborators;
                let total = usize::from(collaborators.codex)
                    + usize::from(collaborators.claude)
                    + collaborators.others;
                if total > 0 {
                    spans.push(Span::styled(
                        "+",
                        Style::default().fg(Color::Rgb(160, 160, 160)),
                    ));
                    let badge = if total == 1 && collaborators.codex {
                        Span::raw("꩜")
                    } else if total == 1 && collaborators.claude {
                        Span::styled("❋", Style::default().fg(Color::Rgb(215, 119, 87)))
                    } else {
                        Span::styled(total.to_string(), Style::default().fg(Color::Gray))
                    };
                    spans.push(badge);
                }
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

/// Consume display options before `--`, leaving traversal and pathspecs for Git.
pub fn parse_args(args: &[String]) -> Result<(LogFormat, Vec<String>), String> {
    let mut format = LogFormat::default();
    let mut git_args = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            git_args.push(arg.clone());
            git_args.extend(args.cloned());
            break;
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
        } else if arg == "--oneline" {
            format = LogFormat::parse("%h (%D) %s")?;
        } else {
            git_args.push(arg.clone());
        }
    }
    Ok((format, git_args))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn commit() -> Commit {
        Commit {
            kind: CommitKind::Revision,
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
        let (compact, _) = parse_args(&["--oneline".into()]).unwrap();
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
    fn parses_options_without_consuming_pathspecs_and_last_format_wins() {
        let args = [
            "--pretty=format:%h %ad %an %s",
            "--date=short",
            "main",
            "--",
            "--format=%b",
        ]
        .map(str::to_owned);
        let (format, args) = parse_args(&args).unwrap();
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
