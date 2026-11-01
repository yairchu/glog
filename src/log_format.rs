use ratatui::{
    style::{Color, Style},
    text::Span,
};
use std::borrow::Cow;

use crate::git::{Agent, Annotation, Collaborators, Commit, CommitKind, ReflogEntry};

const MAX_AUTHOR_WIDTH: usize = 32;
/// Names wider than this show later words as initials in the default Log.
const COMPACT_NAME_WIDTH: usize = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Hash,
    Date,
    Author,
    Refs,
    Subject,
}

impl Field {
    /// Blame rows have no refs or subjects; these columns attribute lines.
    pub const ATTRIBUTION: [Field; 3] = [Field::Hash, Field::Date, Field::Author];

    pub fn for_key(key: char) -> Option<Field> {
        match key {
            'a' => Some(Field::Author),
            'd' => Some(Field::Date),
            'r' => Some(Field::Refs),
            's' => Some(Field::Subject),
            'x' => Some(Field::Hash),
            _ => None,
        }
    }
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
    tabular: bool,
}

/// The default Log's date width, measured once per history. Graph/hash and
/// author widths follow the rows on screen, so each draw supplies them.
#[derive(Clone, Copy)]
pub struct DateColumn(usize);

impl DateColumn {
    pub fn measure(commits: &[Commit]) -> Self {
        Self(
            commits
                .iter()
                .filter(|c| c.kind == CommitKind::Revision && c.annotation.is_none())
                .map(|commit| Span::raw(&commit.author_date).width())
                .max()
                .unwrap_or(0),
        )
    }

    pub fn columns(self, hash: usize, author: usize) -> Columns {
        Columns {
            hash,
            date: self.0,
            author,
        }
    }
}

/// The author column wide enough for these commits' compact labels.
pub fn author_column<'a>(commits: impl IntoIterator<Item = &'a Commit>) -> usize {
    commits
        .into_iter()
        .map(author_width)
        .max()
        .unwrap_or(0)
        .min(MAX_AUTHOR_WIDTH)
}

/// Screen widths for one row of the default Log's metadata.
#[derive(Clone, Copy)]
pub struct Columns {
    hash: usize,
    date: usize,
    author: usize,
}

impl Columns {
    fn width(&self, field: Field) -> Option<usize> {
        match field {
            Field::Hash => Some(self.hash),
            Field::Date => Some(self.date),
            Field::Author => Some(self.author),
            _ => None,
        }
    }
}

impl Default for LogFormat {
    fn default() -> Self {
        let mut format = Self::parse("%h %ad %an (%D) %s").unwrap();
        format.tabular = true;
        format
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
        Ok(Self {
            parts,
            tabular: false,
        })
    }

    pub fn is_tabular(&self) -> bool {
        self.tabular
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

    #[cfg(test)]
    pub fn spans(&self, commit: &Commit) -> Vec<Span<'static>> {
        self.row(commit, None).spans
    }

    pub fn row(&self, commit: &Commit, columns: Option<&Columns>) -> Row {
        let spans = match &commit.annotation {
            Some(Annotation::Reflog(entry)) => reflog_spans(commit, entry),
            Some(Annotation::Stash(entry)) => crate::stash::spans(commit, entry, self),
            Some(Annotation::Blame(line)) => {
                return crate::blame::row(commit, line, line.starts_chunk, self);
            }
            Some(Annotation::Branch(node)) => crate::branches::spans(commit, node, self),
            None => return self.commit_row(commit, columns),
        };
        Row {
            spans,
            abbreviated: Vec::new(),
        }
    }

    fn commit_row(&self, commit: &Commit, columns: Option<&Columns>) -> Row {
        // Synthetic entries must remain identifiable even in author-only formats.
        if commit.kind != CommitKind::Revision {
            let spans = vec![
                Span::styled(
                    format!("{} ", commit.short_hash),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(commit.subject.clone()),
            ];
            return Row {
                spans,
                abbreviated: Vec::new(),
            };
        }
        let mut spans = Vec::new();
        let mut abbreviated = Vec::new();
        // Append badges once, after the last visible author name/email field.
        let last_author = self.parts.iter().rposition(|part| {
            part.enabled
                && match part.token.as_str() {
                    "an" => !commit.author.is_empty(),
                    "ae" => !commit.author_email.is_empty(),
                    _ => false,
                }
        });
        let displayed_author = if columns.is_some() {
            author_label(&commit.author)
        } else {
            Cow::Borrowed(commit.author.as_str())
        };
        for (index, part) in self
            .parts
            .iter()
            .enumerate()
            .filter(|(_, part)| part.enabled)
        {
            let value: &str = match part.token.as_str() {
                "h" => &commit.short_hash,
                "H" => &commit.hash,
                "ad" => &commit.author_date,
                "an" => &displayed_author,
                "ae" => &commit.author_email,
                "d" | "D" => commit.decorations.ansi(),
                _ => &commit.subject,
            };
            let full = if part.token == "an" {
                &commit.author
            } else {
                value
            };
            let width = columns.and_then(|columns| columns.width(part.field));
            if value.is_empty() && width.unwrap_or(0) == 0 {
                continue;
            }
            let value = if part.token == "d" {
                format!(" ({value})")
            } else {
                value.to_owned()
            };
            let badges = if Some(index) == last_author {
                coauthor_badge(&commit.collaborators)
            } else {
                Vec::new()
            };
            let badge_width: usize = badges.iter().map(Span::width).sum();
            let value = match (part.field, width) {
                (Field::Author, Some(width)) => truncate(&value, width.saturating_sub(badge_width)),
                _ => value,
            };
            let padding = width.map_or(0, |width| {
                width.saturating_sub(Span::raw(&value).width() + badge_width)
            });
            let text = format!("{}{value}{}", part.prefix, part.suffix);
            let text = if spans.is_empty() {
                text.trim_start().to_owned()
            } else {
                text
            };
            if part.field == Field::Refs {
                spans.extend(crate::ansi::parse_line(&text).spans);
                continue;
            }
            if value != full {
                let end = spans
                    .iter()
                    .map(|span: &Span| span.content.len())
                    .sum::<usize>()
                    + text.len()
                    - part.suffix.len();
                let range = end - value.len()..end;
                // Search also matches a compact author label cut off here.
                if part.token == "an" && *displayed_author != *full {
                    abbreviated.push((range.clone(), displayed_author.to_string()));
                }
                abbreviated.push((range, full.to_owned()));
            }
            let span = field_span(part.field, text);
            if part.field == Field::Subject
                && crate::log_folds::commit_type_prefix(commit).is_some()
            {
                // Preserve format literals and the subject's original spelling.
                let start = span.content.len() - value.len() - part.suffix.len();
                spans.extend(
                    crate::ansi::style_ranges(
                        ratatui::text::Line::from(span),
                        crate::message_format::subject_ranges(&value, start),
                    )
                    .spans,
                );
            } else {
                spans.push(span);
            }
            spans.extend(badges);
            if padding > 0 {
                spans.push(Span::raw(" ".repeat(padding)));
            }
        }
        Row { spans, abbreviated }
    }

    #[cfg(test)]
    pub fn text(&self, commit: &Commit) -> String {
        self.row(commit, None).text()
    }

    /// Whether a lowercase query matches the row's text, the full values of
    /// any it shows shortened, or the compact author label that a tabular Log
    /// displays in place of a long or bot name.
    pub fn matches(&self, commit: &Commit, query: &str) -> bool {
        let row = self.row(commit, None);
        row.text().to_lowercase().contains(query)
            || row
                .abbreviated
                .iter()
                .any(|(_, full)| full.to_lowercase().contains(query))
            || self.tabular
                && self.shows(Field::Author)
                && commit.kind == CommitKind::Revision
                && commit.annotation.is_none()
                && author_label(&commit.author).to_lowercase().contains(query)
    }
}

/// A Log row's spans, and the byte ranges in them of values shown shortened,
/// with each text they stand for, which search matches.
pub struct Row {
    pub spans: Vec<Span<'static>>,
    pub abbreviated: Vec<(std::ops::Range<usize>, String)>,
}

impl Row {
    pub fn text(&self) -> String {
        self.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}

/// Compact labels present GitHub-style bot names and long names. Leave the
/// stored author untouched for searching, explicit formats, and Show.
pub(crate) fn author_label(author: &str) -> Cow<'_, str> {
    let Some(name) = author.strip_suffix("[bot]") else {
        return compact_name(author);
    };
    let name = match name {
        "copilot-swe-agent" => "Copilot",
        "dependabot" => "Dependabot",
        "" => return Cow::Borrowed("🤖"),
        name => name,
    };
    Cow::Owned(format!("{name} 🤖"))
}

/// Each name's label depends only on the name, so it stays the same
/// whichever rows are shown. Initials replace words after the first, last
/// first, until the name fits; a name that still does not fit is truncated
/// with the column.
fn compact_name(name: &str) -> Cow<'_, str> {
    if Span::raw(name).width() <= COMPACT_NAME_WIDTH {
        return Cow::Borrowed(name);
    }
    let mut words: Vec<Cow<str>> = name.split_whitespace().map(Cow::Borrowed).collect();
    for index in (1..words.len()).rev() {
        let word = Span::raw(words[index].to_string());
        let mut graphemes = word.styled_graphemes(Style::default());
        let first = graphemes.next().map(|grapheme| grapheme.symbol.to_owned());
        // Words as short as an initial, such as `B.` or `Jr`, stay.
        if let Some(first) = first.filter(|_| graphemes.nth(1).is_some()) {
            words[index] = Cow::Owned(format!("{first}."));
        }
        if Span::raw(words.join(" ")).width() <= COMPACT_NAME_WIDTH {
            break;
        }
    }
    Cow::Owned(words.join(" "))
}

fn author_width(commit: &Commit) -> usize {
    let badges: usize = coauthor_badge(&commit.collaborators)
        .iter()
        .map(Span::width)
        .sum();
    Span::raw(author_label(&commit.author)).width() + badges
}

/// Shorten text to a display width, ending it with an ellipsis.
/// Graphemes stay whole, so combining marks keep their base letters.
pub fn truncate(text: &str, width: usize) -> String {
    let span = Span::raw(text);
    if span.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in span.styled_graphemes(Style::default()) {
        let next = Span::raw(grapheme.symbol).width();
        if used + next > width - 1 {
            break;
        }
        result.push_str(grapheme.symbol);
        used += next;
    }
    result.push('…');
    result
}

/// An agent's symbol and its color, or None for the terminal foreground.
fn agent_symbol(agent: Agent) -> (&'static str, Option<(u8, u8, u8)>) {
    match agent {
        Agent::Codex => ("꩜", None),
        Agent::Claude => ("❋", Some((215, 119, 87))),
        Agent::Gemini => ("✦", Some((66, 133, 244))),
    }
}

fn agent_icon(agent: Agent) -> Span<'static> {
    match agent_symbol(agent) {
        (symbol, None) => Span::raw(symbol),
        (symbol, Some((r, g, b))) => Span::styled(symbol, Style::default().fg(Color::Rgb(r, g, b))),
    }
}

/// The agent's icon as ANSI text, for views rendered from Git's output.
pub fn agent_icon_ansi(agent: Agent) -> String {
    match agent_symbol(agent) {
        (symbol, None) => symbol.to_owned(),
        (symbol, Some((r, g, b))) => format!("\x1b[38;2;{r};{g};{b}m{symbol}\x1b[m"),
    }
}

/// A `+` and a symbol for an agent coauthor, or the number of coauthors.
pub fn coauthor_badge(collaborators: &Collaborators) -> Vec<Span<'static>> {
    let mut agents = [
        (Agent::Codex, collaborators.codex),
        (Agent::Claude, collaborators.claude),
        (Agent::Gemini, collaborators.gemini),
    ]
    .into_iter()
    .filter_map(|(agent, present)| present.then_some(agent));
    let first = agents.next();
    let total = usize::from(first.is_some()) + agents.count() + collaborators.others;
    if total == 0 {
        return Vec::new();
    }
    let badge = match (total, first) {
        (1, Some(agent)) => agent_icon(agent),
        _ => Span::styled(total.to_string(), Style::default().fg(Color::Gray)),
    };
    vec![
        Span::styled("+", Style::default().fg(Color::Rgb(160, 160, 160))),
        badge,
    ]
}

/// Pad `text` to a column `width` wide, then separate it from the next one.
pub(crate) fn pad(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(Span::raw(text).width());
    format!("{text}{} ", " ".repeat(padding))
}

/// Shared styling for fields in Log and the branch graph.
pub(crate) fn field_span(field: Field, text: String) -> Span<'static> {
    let style = match field {
        Field::Hash => Style::default().fg(Color::Yellow),
        Field::Date => Style::default().fg(Color::Gray),
        Field::Author => Style::default().fg(Color::Cyan),
        Field::Refs | Field::Subject => Style::default(),
    };
    Span::styled(text, style)
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
        if let Some(references) = reflog_reference_spans(action, message) {
            spans.extend(references);
        } else if crate::log_folds::subject_type_prefix(message).is_some() {
            spans.extend(
                crate::ansi::style_ranges(
                    ratatui::text::Line::raw(message.to_owned()),
                    crate::message_format::subject_ranges(message, 0),
                )
                .spans,
            );
        } else {
            spans.push(Span::raw(message.to_owned()));
        }
    } else {
        spans.push(Span::raw(entry.action.clone()));
    }
    spans
}

/// Parse Git's structured action messages without depending on surviving refs.
fn reflog_reference_spans(action: &str, message: &str) -> Option<Vec<Span<'static>>> {
    use Reference::*;
    let (prefix, from, to) = match (action, crate::log_folds::rebase_stage(action)) {
        ("checkout", _) => ("moving from ", Some(Current), Argument),
        ("Branch", _) => ("renamed ", Some(Current), Current),
        ("branch", _) => ("Created from ", None, Argument),
        ("reset", _) => ("moving to ", None, Argument),
        (_, Some("start")) => ("checkout ", None, Argument),
        (_, Some("finish" | "abort")) => ("returning to ", None, Current),
        _ => return None,
    };
    let references = message.strip_prefix(prefix)?;
    let valid =
        |reference: &str| !reference.is_empty() && !reference.chars().any(char::is_whitespace);
    let mut spans = vec![Span::raw(prefix)];
    if let Some(from) = from {
        let (source, target) = references.split_once(" to ")?;
        if !valid(source) || !valid(target) {
            return None;
        }
        spans.extend([from.span(source), Span::raw(" to "), to.span(target)]);
    } else {
        if !valid(references) {
            return None;
        }
        spans.push(to.span(references));
    }
    Some(spans)
}

/// How Git wrote a reference into a reflog message.
#[derive(Clone, Copy)]
enum Reference {
    /// A ref name, or the full hash of a detached HEAD.
    Current,
    /// The revision as the user typed it, so a hex name may be a branch.
    Argument,
}

impl Reference {
    fn span(self, reference: &str) -> Span<'static> {
        let length = reference.len();
        let hash = match self {
            Self::Current => matches!(length, 40 | 64),
            Self::Argument => (7..=64).contains(&length),
        } && reference.bytes().all(|byte| byte.is_ascii_hexdigit());
        Span::styled(
            reference.to_owned(),
            Style::default().fg(if hash { Color::Yellow } else { Color::Green }),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogOptions {
    pub exit_on_esc: bool,
    pub fold_merges: bool,
    pub ascii_graph: bool,
    pub hide_merges: bool,
    pub hidden_types: std::collections::BTreeSet<String>,
    pub watch: bool,
}

/// Git consumes the following argument verbatim as a search pattern.
pub(crate) fn takes_pattern(arg: &str) -> bool {
    matches!(
        arg,
        "--grep" | "--grep-reflog" | "--author" | "--committer" | "-G" | "-S"
    )
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
        if takes_pattern(arg) {
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
                options.hidden_types.insert(kind.into_owned());
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
        } else if arg == "--exit-on-esc" {
            options.exit_on_esc = true;
        } else if arg == "--watch" {
            options.watch = true;
        } else if arg == "--ascii-graph" {
            options.ascii_graph = true;
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
                rebasing: false,
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
                    .filter(|span| {
                        span.style.fg == Some(color)
                            && !span
                                .style
                                .add_modifier
                                .contains(ratatui::style::Modifier::BOLD)
                    })
                    .map(|span| span.content.as_ref())
                    .collect();
                assert_eq!(parts, expected.into_iter().collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn reflog_structured_messages_highlight_only_reference_targets() {
        for (action, targets) in [
            (
                "checkout: moving from deleted/topic to feature/café",
                vec![
                    ("deleted/topic", Color::Green),
                    ("feature/café", Color::Green),
                ],
            ),
            (
                "checkout: moving from 0123456789abcdef0123456789abcdef01234567 to main",
                vec![
                    ("0123456789abcdef0123456789abcdef01234567", Color::Yellow),
                    ("main", Color::Green),
                ],
            ),
            (
                "checkout: moving from main to abc1234",
                vec![("main", Color::Green), ("abc1234", Color::Yellow)],
            ),
            (
                "checkout: moving from 1234567 to main",
                vec![("1234567", Color::Green), ("main", Color::Green)],
            ),
            (
                "Branch: renamed refs/heads/old to refs/heads/new",
                vec![
                    ("refs/heads/old", Color::Green),
                    ("refs/heads/new", Color::Green),
                ],
            ),
            ("branch: Created from HEAD", vec![("HEAD", Color::Green)]),
            ("reset: moving to HEAD~1", vec![("HEAD~1", Color::Green)]),
            ("reset: moving to abc1234", vec![("abc1234", Color::Yellow)]),
            (
                "rebase (start): checkout main",
                vec![("main", Color::Green)],
            ),
            (
                "rebase (finish): returning to refs/heads/topic",
                vec![("refs/heads/topic", Color::Green)],
            ),
            (
                "rebase -i (start): checkout main",
                vec![("main", Color::Green)],
            ),
            (
                "rebase (continue) (finish): returning to refs/heads/topic",
                vec![("refs/heads/topic", Color::Green)],
            ),
            (
                "pull -r origin main -q (start): checkout abc1234",
                vec![("abc1234", Color::Yellow)],
            ),
            (
                "rebase (abort): returning to refs/heads/topic",
                vec![("refs/heads/topic", Color::Green)],
            ),
            (
                "pull --rebase (finish): returning to refs/heads/main",
                vec![("refs/heads/main", Color::Green)],
            ),
            ("rebase (pick): checkout main", vec![]),
            ("commit: moving from main to topic", vec![]),
            ("checkout: moving from main to topic extra text", vec![]),
            ("checkout: moving from main to ", vec![]),
            ("checkout: moving from to topic", vec![]),
            ("reset: moving to ", vec![]),
            ("reset: moving to main with extra text", vec![]),
            ("custom: main to topic", vec![]),
        ] {
            let mut c = commit();
            c.annotation = Some(Annotation::Reflog(ReflogEntry {
                reference: "HEAD".into(),
                show_reference: false,
                rebasing: false,
                updated_at: "2026-10-12 10:00:01".into(),
                actor: "Alice".into(),
                action: action.into(),
            }));
            let spans = LogFormat::default().spans(&c);
            assert_eq!(
                spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                format!("{} 2026-10-12 10:00:01 Alice {action}", c.short_hash)
            );
            let highlighted: Vec<_> = spans
                .iter()
                .skip(3)
                .filter_map(|span| match span.style.fg {
                    Some(color @ (Color::Green | Color::Yellow)) => {
                        Some((span.content.as_ref(), color))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(highlighted, targets, "{action}");
        }
    }

    #[test]
    fn breaking_markers_survive_custom_formats_and_case_insensitive_type_filters() {
        let mut c = commit();
        c.subject = "Feat(api)!: new API".into();
        for source in ["%s", "«%s»", "%s / %s"] {
            let format = LogFormat::parse(source).unwrap();
            let spans = format.spans(&c);
            let markers: Vec<_> = spans.iter().filter(|span| span.content == "!").collect();
            assert_eq!(markers.len(), source.matches("%s").count());
            assert!(markers
                .iter()
                .all(|span| span.style == crate::message_format::breaking_style()));
            assert_eq!(format.text(&c), source.replace("%s", &c.subject));
        }
        let mut reflog = c.clone();
        reflog.annotation = Some(Annotation::Reflog(ReflogEntry {
            reference: "HEAD".into(),
            show_reference: false,
            rebasing: false,
            updated_at: "2026-10-12".into(),
            actor: "Alice".into(),
            action: format!("commit: {}", c.subject),
        }));
        assert!(LogFormat::default().spans(&reflog).iter().any(|span| {
            span.content == "!" && span.style == crate::message_format::breaking_style()
        }));
        let (_, options, _) =
            parse_args(&["--hide-types=FIX,DoC,TESTS,Custom-Type".into()]).unwrap();
        assert_eq!(
            options.hidden_types,
            ["fix", "docs", "test", "custom-type"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        c.decorations.clear();
        let mut folds = crate::log_folds::LogFolds::default();
        folds.refresh(&[c.clone()]).unwrap();
        folds.toggle_type(Some("FEATURE"), &[c.clone()]).unwrap();
        assert!(folds.filter_hidden(0, &c));
        folds.toggle_type(Some("feat"), &[c]).unwrap();
        assert!(folds.hidden_types.is_empty());
    }

    #[test]
    fn subject_types_keep_original_spelling_and_format_literals() {
        for (subject, kind) in [
            ("feat(log)!: add filtering", Some("feat")),
            ("FIX(api)!: repair", Some("FIX")),
            ("DoCs: explain", Some("DoCs")),
            ("tests #234(failing): reproduce crash", Some("tests")),
            ("test(failing)(dsp): reproduce crash", Some("test")),
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
            "--ascii-graph",
            "--all",
        ]
        .map(str::to_owned);
        let (_, options, git) = parse_args(&args).unwrap();
        assert!(options.hide_merges && options.fold_merges && options.ascii_graph);
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
    fn exit_on_esc_preserves_git_patterns_and_paths() {
        for (args, enabled, forwarded) in [
            (
                vec!["--exit-on-esc", "main..HEAD"],
                true,
                vec!["main..HEAD"],
            ),
            (vec!["--watch", "--exit-on-esc"], true, vec![]),
            (
                vec!["--grep", "--exit-on-esc"],
                false,
                vec!["--grep", "--exit-on-esc"],
            ),
            (
                vec!["--", "--exit-on-esc"],
                false,
                vec!["--", "--exit-on-esc"],
            ),
            (vec!["--format", "--exit-on-esc %s"], false, vec![]),
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let (_, options, git) = parse_args(&args).unwrap();
            assert_eq!(options.exit_on_esc, enabled);
            assert_eq!(git, forwarded);
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
            first_parent_only: false,
            graph_node: '*',
        }
    }

    #[test]
    fn only_single_recognized_coauthors_use_icons() {
        for (codex, claude, gemini, others, expected) in [
            (false, false, false, 0, "Alice"),
            (true, false, false, 0, "Alice+꩜"),
            (false, true, false, 0, "Alice+❋"),
            (false, false, true, 0, "Alice+✦"),
            (false, false, false, 1, "Alice+1"),
            (true, true, false, 0, "Alice+2"),
            (false, true, false, 1, "Alice+2"),
            (true, false, false, 1, "Alice+2"),
            (false, false, true, 1, "Alice+2"),
            (true, false, true, 0, "Alice+2"),
            (false, true, true, 0, "Alice+2"),
            (false, false, false, 2, "Alice+2"),
            (true, true, false, 2, "Alice+4"),
            (true, true, true, 2, "Alice+5"),
        ] {
            let mut c = commit();
            c.collaborators = crate::git::Collaborators {
                codex,
                claude,
                gemini,
                others,
                ..Default::default()
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
            ..Default::default()
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

    /// Columns for rendering every commit, as if all were on screen.
    fn all_columns(commits: &[Commit]) -> Columns {
        let hash = commits
            .iter()
            .map(|commit| Span::raw(&commit.short_hash).width())
            .max()
            .unwrap_or(0);
        DateColumn::measure(commits).columns(hash, author_column(commits))
    }

    #[test]
    fn long_names_show_later_words_as_initials() {
        for (name, label) in [
            ("Yair Chuchem", "Yair Chuchem"),
            ("Margaret Hamilton", "Margaret H."),
            ("Ludwig van Beethoven", "Ludwig van B."),
            ("Johann Sebastian Bachmann", "Johann S. B."),
            ("Bartholomew-Alexander", "Bartholomew-Alexander"),
        ] {
            assert_eq!(author_label(name), label);
        }
    }

    #[test]
    fn bot_author_labels_shrink_columns_without_losing_raw_names_or_badges() {
        let cases = [
            ("copilot-swe-agent[bot]", "Copilot 🤖"),
            ("dependabot[bot]", "Dependabot 🤖"),
            ("release[bot]", "release 🤖"),
            ("Yair Chuchem", "Yair Chuchem"),
            ("Copilot", "Copilot"),
        ];
        let commits: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(index, (name, _))| {
                let mut commit = commit();
                commit.author = (*name).into();
                commit.subject = format!("subject-{index}");
                commit.decorations.clear();
                commit.collaborators.codex = index == 0;
                commit
            })
            .collect();
        let format = LogFormat::default();
        let columns = all_columns(&commits);
        assert_eq!(columns.author, Span::raw("Dependabot 🤖").width());
        assert!(columns.author < Span::raw(cases[0].0).width());
        let mut subject_column = None;
        for (commit, (original, label)) in commits.iter().zip(cases) {
            let rendered: String = format
                .row(commit, Some(&columns))
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert!(rendered.contains(label), "{rendered}");
            let column = Span::raw(&rendered[..rendered.find("subject-").unwrap()]).width();
            assert_eq!(column, *subject_column.get_or_insert(column));
            assert!(format.text(commit).contains(original));
            assert!(LogFormat::parse("%an <%ae> %s")
                .unwrap()
                .text(commit)
                .starts_with(&format!("{original} <")));
        }
        let rendered: String = format
            .row(&commits[0], Some(&columns))
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains("Copilot 🤖+꩜"));

        let mut app = crate::app::App::new(commits);
        app.selected = 1;
        app.search = Some("copilot-swe-agent[bot]".into());
        app.next_match(false);
        assert_eq!(app.selected, 0);
        // Search also finds the label it highlights.
        app.search = Some("Dependabot 🤖".into());
        app.next_match(false);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn default_columns_align_subject_heads_with_unicode_names_and_badges() {
        let mut commits = vec![commit(), commit(), commit()];
        for (index, commit) in commits.iter_mut().enumerate() {
            commit.subject = format!("subject-{index}");
            commit.decorations.clear();
        }
        commits[0].author = "Al".into();
        commits[0].decorations = "HEAD -> main".into();
        commits[0].collaborators.codex = true;
        commits[1].author = "李 e\u{301}".into();
        commits[1].short_hash.push_str("12");
        commits[1].author_date = "Nov 1".into();
        commits[1].collaborators.others = 12;
        commits[2].author = "👩‍💻".repeat(24);
        commits[2].collaborators.claude = true;
        let format = LogFormat::default();
        let columns = all_columns(&commits);
        let rendered: Vec<String> = commits
            .iter()
            .map(|commit| {
                format
                    .row(commit, Some(&columns))
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let starts: Vec<_> = rendered
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let start = text
                    .find(if index == 0 { "(HEAD" } else { "subject-" })
                    .unwrap();
                Span::raw(&text[..start]).width()
            })
            .collect();
        assert!(starts.iter().all(|column| *column == starts[0]));
        assert!(rendered[0].ends_with("(HEAD -> main) subject-0"));
        assert!(rendered[0].contains("Al+꩜"));
        assert!(rendered[1].contains("李 e\u{301}+12"));
        assert!(rendered[2].contains("👩‍💻…+❋"));
        assert!(!rendered[2].contains(&commits[2].author));
        // Truncation is only visual: full author names remain searchable.
        assert!(format.text(&commits[2]).contains(&commits[2].author));

        let explicit = LogFormat::parse("%an (%D) %s").unwrap();
        assert!(!explicit.is_tabular());
        assert_eq!(explicit.text(&commits[0]), "Al+꩜ (HEAD -> main) subject-0");
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
