//! Line positions within unified and combined diff hunks.

use crate::ansi;

/// The plain-text lines of a file's patch before its first hunk.
pub fn file_headers(patch: &str) -> impl Iterator<Item = String> + '_ {
    patch
        .lines()
        .map(ansi::plain)
        .take_while(|line| !line.starts_with("@@"))
}

/// A one-based start line and line count.
pub type Range = (u32, u32);

pub struct Header {
    /// One range per parent; combined diffs have several.
    pub old: Vec<Range>,
    pub new: Range,
}

pub fn header(line: &str) -> Option<Header> {
    let mut fields = line.split_whitespace();
    if !fields.next()?.starts_with("@@") {
        return None;
    }
    let mut old = Vec::new();
    for field in fields {
        if let Some(text) = field.strip_prefix('-') {
            old.push(range(text)?);
        } else {
            let new = range(field.strip_prefix('+')?)?;
            return (!old.is_empty()).then_some(Header { old, new });
        }
    }
    None
}

fn range(text: &str) -> Option<Range> {
    let (start, count) = text.split_once(',').unwrap_or((text, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

pub enum Row {
    Header,
    Line {
        /// Context and additions survive in the result; deletions do not.
        result: bool,
        /// Whether each parent contains the line.
        parents: Vec<bool>,
    },
    /// Not a file line, such as "\ No newline at end of file".
    Other,
}

pub struct Position {
    pub row: Row,
    /// The line number in each parent of this row or, if it is absent from
    /// that parent, of the next line that is present.
    pub old: Vec<u32>,
    /// The same for the result.
    pub new: u32,
}

/// The position of a patch row, or None outside hunks.
pub fn position(patch: &str, row: usize) -> Option<Position> {
    let mut lines: Option<(Vec<u32>, u32)> = None;
    for (index, text) in patch.lines().enumerate().take(row.saturating_add(1)) {
        let text = ansi::plain(text);
        let kind = if text.starts_with("@@") {
            lines = header(&text)
                .map(|Header { old, new }| (old.iter().map(|&(start, _)| start).collect(), new.0));
            Row::Header
        } else {
            let Some((old, _)) = &lines else {
                continue;
            };
            classify(&text, old.len())
        };
        let Some((old, new)) = lines.as_mut() else {
            continue;
        };
        if index == row {
            return Some(Position {
                row: kind,
                old: old.clone(),
                new: *new,
            });
        }
        if let Row::Line { result, parents } = kind {
            for (number, present) in old.iter_mut().zip(parents) {
                *number = number.saturating_add(u32::from(present));
            }
            *new = new.saturating_add(u32::from(result));
        }
    }
    None
}

fn classify(text: &str, parents: usize) -> Row {
    // diff.suppressBlankEmpty prints blank context lines without a space.
    if text.is_empty() {
        return Row::Line {
            result: true,
            parents: vec![true; parents],
        };
    }
    let Some(prefix) = text
        .as_bytes()
        .get(..parents)
        .filter(|prefix| prefix.iter().all(|b| matches!(b, b' ' | b'+' | b'-')))
    else {
        return Row::Other;
    };
    // A deletion belongs only to the parents marked '-'. Result lines
    // belong to each parent whose column is unchanged.
    let result = !prefix.contains(&b'-');
    let marker = if result { b' ' } else { b'-' };
    Row::Line {
        result,
        parents: prefix.iter().map(|&b| b == marker).collect(),
    }
}
