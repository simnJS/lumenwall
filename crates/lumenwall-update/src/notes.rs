//! Release notes for a small panel: GitHub Markdown reduced to plain text.
//!
//! Headings lose their `#`, list items become `•`, links keep their text, emphasis and
//! code marks go, HTML comments (GitHub's generated-notes marker) are dropped, and the
//! repository's pull-request and compare URLs shrink to `#12` and `v0.1.0…v0.2.0`.
//! The release page's download section (between [`DOWNLOADS_START`] and
//! [`DOWNLOADS_END`], `packaging/release/release-notes.md`) is skipped: the app is
//! already installed. The text is shown as text, never interpreted.

/// Opens the download section the release workflow puts before the generated notes.
pub const DOWNLOADS_START: &str = "<!-- downloads -->";
/// Closes it.
pub const DOWNLOADS_END: &str = "<!-- /downloads -->";

/// One line of release notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Heading(String),
    /// A list item; `depth` 0 for top-level items.
    Item {
        depth: usize,
        text: String,
    },
    Text(String),
    /// Paragraph break (never first, last, or repeated).
    Blank,
}

impl std::fmt::Display for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Line::Heading(t) | Line::Text(t) => f.write_str(t),
            Line::Item { depth, text } => write!(f, "{}• {text}", "  ".repeat(*depth)),
            Line::Blank => Ok(()),
        }
    }
}

/// The lines of `markdown`, at most `max_lines` of them (then a `…` line marks the cut).
pub fn lines(markdown: &str, max_lines: usize) -> Vec<Line> {
    let text = strip_comments(&without_downloads(markdown));
    let mut out: Vec<Line> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        let indent = line.len() - line.trim_start().len();
        let body = line.trim_start();
        if body.is_empty() {
            if out.last().is_some_and(|l| *l != Line::Blank) {
                out.push(Line::Blank);
            }
            continue;
        }
        if body.chars().all(|c| matches!(c, '-' | '*' | '_' | ' ')) && body.len() >= 3 {
            continue;
        }
        out.push(if let Some(h) = heading(body) {
            Line::Heading(inline(h))
        } else if let Some(item) = ["* ", "- ", "+ "].iter().find_map(|m| body.strip_prefix(m)) {
            Line::Item {
                depth: (indent / 2).min(3),
                text: inline(item),
            }
        } else {
            Line::Text(inline(body))
        });
    }
    while out.last() == Some(&Line::Blank) {
        out.pop();
    }
    if out.len() > max_lines {
        out.truncate(max_lines);
        while out.last() == Some(&Line::Blank) {
            out.pop();
        }
        out.push(Line::Text("…".into()));
    }
    out
}

/// Plain-text version of `markdown` (see [`lines`]).
pub fn plain_text(markdown: &str, max_lines: usize) -> String {
    lines(markdown, max_lines)
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `s` without its download section (kept whole when the end marker is missing).
fn without_downloads(s: &str) -> String {
    if let Some(start) = s.find(DOWNLOADS_START)
        && let Some(len) = s[start..].find(DOWNLOADS_END)
    {
        let end = start + len + DOWNLOADS_END.len();
        return format!("{}{}", &s[..start], &s[end..]);
    }
    s.to_owned()
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn heading(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    (1..=6)
        .contains(&hashes)
        .then(|| line[hashes..].trim())
        .filter(|_| line[hashes..].starts_with(' ') || line.len() == hashes)
}

/// Links to their text, emphasis and code marks removed, GitHub URLs shortened.
fn inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        // `[text](url)` and `![alt](url)`.
        let image = rest.starts_with("![");
        if (image || rest.starts_with('['))
            && let Some((text, after)) = link(&rest[usize::from(image)..])
        {
            out.push_str(text);
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")) {
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('`') {
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('<')
            && let Some(end) = after.find('>')
            && after[..end].starts_with("http")
        {
            out.push_str(&after[..end]);
            rest = &after[end + 1..];
            continue;
        }
        let c = rest.chars().next().unwrap_or_default();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    shorten_urls(&out)
}

/// `[text](url)rest` → (`text`, `rest`).
fn link(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix('[')?;
    let close = s.find("](")?;
    let text = &s[..close];
    if text.contains(['[', ']']) {
        return None;
    }
    let after = &s[close + 2..];
    let end = after.find(')')?;
    Some((text, &after[end + 1..]))
}

fn shorten_urls(s: &str) -> String {
    s.split(' ')
        .map(|word| {
            let Some(path) = word.strip_prefix("https://github.com/") else {
                return word.to_owned();
            };
            let parts: Vec<&str> = path.splitn(4, '/').collect();
            match parts.as_slice() {
                [_, _, "pull" | "issues", n]
                    if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    format!("#{n}")
                }
                [_, _, "compare", range] if range.contains("...") => range.replace("...", "…"),
                _ => word.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_generated_notes() {
        let md = [
            "<!-- Release notes generated using configuration in .github/release.yml at v0.2.0 -->",
            "",
            "## What's Changed",
            "### Engine",
            "* Faster **scene** loading by @someone in https://github.com/simnJS/lumenwall/pull/12",
            "* Fix `--fps` parsing in https://github.com/simnJS/lumenwall/issues/7",
            "  - nested [detail](https://example.com/x)",
            "",
            "",
            "---",
            "**Full Changelog**: https://github.com/simnJS/lumenwall/compare/v0.1.0...v0.2.0",
            "",
        ]
        .join("\r\n");
        let want = [
            "What's Changed",
            "Engine",
            "• Faster scene loading by @someone in #12",
            "• Fix --fps parsing in #7",
            "  • nested detail",
            "",
            "Full Changelog: v0.1.0…v0.2.0",
        ]
        .join("\n");
        assert_eq!(plain_text(&md, 20), want);
        let l = lines(&md, 20);
        assert_eq!(l[0], Line::Heading("What's Changed".into()));
        assert_eq!(
            l[4],
            Line::Item {
                depth: 1,
                text: "nested detail".into()
            }
        );
        assert_eq!(l[5], Line::Blank);
    }

    #[test]
    fn truncates_and_keeps_text() {
        let md = "a\nb\nc\nd";
        assert_eq!(plain_text(md, 2), "a\nb\n…");
        assert_eq!(
            plain_text("#hashtag and [not a link", 5),
            "#hashtag and [not a link"
        );
        assert_eq!(
            plain_text("![logo](x.png) <https://a.example>", 5),
            "logo https://a.example"
        );
        assert_eq!(plain_text("", 5), "");
        assert_eq!(plain_text("x <!-- never closed", 5), "x");
    }

    #[test]
    fn skips_the_download_section() {
        // What the release workflow publishes: the template, then GitHub's notes.
        let template = include_str!("../../../packaging/release/release-notes.md")
            .replace("@VERSION@", "0.2.0");
        let md = format!(
            "{template}<!-- Release notes generated using configuration in .github/release.yml at v0.2.0 -->\n\n## What's Changed\n* Faster in https://github.com/simnJS/lumenwall/pull/12\n"
        );
        assert!(md.contains("Lumenwall-Setup.exe"));
        assert_eq!(plain_text(&md, 5), "What's Changed\n• Faster in #12");
        // Without its end marker, nothing is hidden.
        let open = "<!-- downloads -->\n## Download\nWindows";
        assert_eq!(plain_text(open, 5), "Download\nWindows");
    }
}
