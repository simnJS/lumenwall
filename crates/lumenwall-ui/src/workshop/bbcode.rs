//! Steam BBCode (Workshop descriptions) to simple rich text: paragraphs, headings,
//! list items and quotes made of styled spans. No HTML, no images, nothing executed;
//! unknown tags are dropped and their text kept.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading,
    Bullet,
    Quote,
    Code,
    Rule,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub spoiler: bool,
    /// Target of `[url]`, only `http(s)` and `steam:` links are kept.
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
}

#[derive(Default, Clone)]
struct Style {
    bold: u8,
    italic: u8,
    underline: u8,
    strike: u8,
    spoiler: u8,
    link: Option<String>,
}

/// Longest description rendered (characters); the rest is cut with an ellipsis.
const MAX_CHARS: usize = 8000;

pub fn parse(src: &str) -> Vec<Block> {
    let src: String = if src.chars().count() > MAX_CHARS {
        let mut s: String = src.chars().take(MAX_CHARS).collect();
        s.push('…');
        s
    } else {
        src.to_owned()
    };
    let src = src.replace("\r\n", "\n");
    let mut blocks: Vec<Block> = Vec::new();
    let mut cur = Block {
        kind: BlockKind::Paragraph,
        spans: Vec::new(),
    };
    let mut style = Style::default();
    let mut kind_stack: Vec<BlockKind> = Vec::new();
    let mut noparse = false;
    let mut chars = src.char_indices().peekable();
    let bytes = src.as_str();

    let flush = |blocks: &mut Vec<Block>, cur: &mut Block, next: BlockKind| {
        let has_text = cur.spans.iter().any(|s| !s.text.trim().is_empty());
        if has_text || cur.kind == BlockKind::Rule {
            // Trim the edges of the block.
            if let Some(first) = cur.spans.first_mut() {
                first.text = first.text.trim_start().to_owned();
            }
            if let Some(last) = cur.spans.last_mut() {
                last.text = last.text.trim_end().to_owned();
            }
            blocks.push(std::mem::replace(
                cur,
                Block {
                    kind: next,
                    spans: Vec::new(),
                },
            ));
        } else {
            cur.spans.clear();
            cur.kind = next;
        }
    };
    let push_text = |cur: &mut Block, style: &Style, text: &str| {
        if text.is_empty() {
            return;
        }
        let span = Span {
            text: String::new(),
            bold: style.bold > 0,
            italic: style.italic > 0,
            underline: style.underline > 0,
            strike: style.strike > 0,
            spoiler: style.spoiler > 0,
            link: style.link.clone(),
        };
        match cur.spans.last_mut() {
            Some(last)
                if Span {
                    text: String::new(),
                    ..last.clone()
                } == span =>
            {
                last.text.push_str(text)
            }
            _ => cur.spans.push(Span {
                text: text.to_owned(),
                ..span
            }),
        }
    };

    while let Some((i, c)) = chars.next() {
        if c == '[' {
            let rest = &bytes[i..];
            if let Some(end) = rest.find(']').filter(|e| *e < 120) {
                let tag = &rest[1..end];
                let (closing, body) = match tag.strip_prefix('/') {
                    Some(b) => (true, b),
                    None => (false, tag),
                };
                let (name, arg) = match body.split_once('=') {
                    Some((n, a)) => (
                        n.trim().to_ascii_lowercase(),
                        Some(a.trim().trim_matches('"').to_owned()),
                    ),
                    None => (body.trim().to_ascii_lowercase(), None),
                };
                if noparse && !(closing && name == "noparse") {
                    push_text(&mut cur, &style, "[");
                    continue;
                }
                let known = handle_tag(
                    &name,
                    arg,
                    closing,
                    &mut style,
                    &mut noparse,
                    &mut cur,
                    &mut blocks,
                    &mut kind_stack,
                    &flush,
                );
                if known {
                    // Skip to the closing bracket (byte offsets: tags may hold UTF-8).
                    while chars.next_if(|(j, _)| *j <= i + end).is_some() {}
                    continue;
                }
            }
            push_text(&mut cur, &style, "[");
            continue;
        }
        if c == '\n' {
            // A blank line ends a paragraph; a single newline inside lists or quotes too.
            let blank = chars.peek().is_some_and(|(_, n)| *n == '\n');
            if blank || cur.kind != BlockKind::Paragraph {
                let next = kind_stack
                    .last()
                    .copied()
                    .filter(|k| *k != BlockKind::Bullet)
                    .unwrap_or(BlockKind::Paragraph);
                flush(&mut blocks, &mut cur, next);
            } else {
                push_text(&mut cur, &style, "\n");
            }
            continue;
        }
        let mut buf = [0u8; 4];
        push_text(&mut cur, &style, c.encode_utf8(&mut buf));
    }
    flush(&mut blocks, &mut cur, BlockKind::Paragraph);
    blocks
}

#[allow(clippy::too_many_arguments)]
fn handle_tag(
    name: &str,
    arg: Option<String>,
    closing: bool,
    style: &mut Style,
    noparse: &mut bool,
    cur: &mut Block,
    blocks: &mut Vec<Block>,
    kinds: &mut Vec<BlockKind>,
    flush: &dyn Fn(&mut Vec<Block>, &mut Block, BlockKind),
) -> bool {
    let counter = |c: &mut u8| {
        if closing {
            *c = c.saturating_sub(1);
        } else {
            *c = c.saturating_add(1);
        }
    };
    match name {
        "b" | "strong" => counter(&mut style.bold),
        "i" | "em" => counter(&mut style.italic),
        "u" => counter(&mut style.underline),
        "strike" | "s" => counter(&mut style.strike),
        "spoiler" => counter(&mut style.spoiler),
        "noparse" => *noparse = !closing,
        "url" => {
            if closing {
                style.link = None;
            } else {
                style.link = arg.filter(|u| {
                    let l = u.to_ascii_lowercase();
                    l.starts_with("https://")
                        || l.starts_with("http://")
                        || l.starts_with("steam://")
                });
            }
        }
        "h1" | "h2" | "h3" => {
            if closing {
                flush(
                    blocks,
                    cur,
                    kinds.last().copied().unwrap_or(BlockKind::Paragraph),
                );
            } else {
                flush(blocks, cur, BlockKind::Heading);
            }
        }
        "list" | "olist" => {
            if closing {
                kinds.pop();
                flush(
                    blocks,
                    cur,
                    kinds.last().copied().unwrap_or(BlockKind::Paragraph),
                );
            } else {
                kinds.push(BlockKind::Bullet);
                flush(blocks, cur, BlockKind::Paragraph);
            }
        }
        "*" => flush(blocks, cur, BlockKind::Bullet),
        "quote" | "code" => {
            let kind = if name == "quote" {
                BlockKind::Quote
            } else {
                BlockKind::Code
            };
            if closing {
                kinds.pop();
                flush(
                    blocks,
                    cur,
                    kinds.last().copied().unwrap_or(BlockKind::Paragraph),
                );
            } else {
                kinds.push(kind);
                flush(blocks, cur, kind);
            }
        }
        "hr" => {
            if !closing {
                flush(blocks, cur, BlockKind::Rule);
                flush(blocks, cur, BlockKind::Paragraph);
            }
        }
        // Images and embeds are not rendered; tables flatten to their text.
        "img" | "previewyoutube" | "previewicon" | "emoticon" => {}
        "table" | "tr" | "th" | "td" | "p" | "center" | "left" | "right" | "color" | "size"
        | "font" => {}
        _ => return false,
    }
    true
}

/// Plain text of the blocks.
#[cfg(test)]
pub fn plain(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|b| b.spans.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_links_and_structure() {
        let b = parse(
            "[h1]Title[/h1]Hello [b]bold [i]both[/i][/b] [url=https://x.org]link[/url].\n\n[list][*]one[*]two[/list][hr][/hr]end",
        );
        let kinds: Vec<BlockKind> = b.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Heading,
                BlockKind::Paragraph,
                BlockKind::Bullet,
                BlockKind::Bullet,
                BlockKind::Rule,
                BlockKind::Paragraph
            ]
        );
        let p = &b[1].spans;
        assert_eq!(p[0].text, "Hello ");
        assert!(p[1].bold && !p[1].italic);
        assert!(p[2].bold && p[2].italic);
        let link = p.iter().find(|s| s.link.is_some()).unwrap();
        assert_eq!(
            (link.text.as_str(), link.link.as_deref()),
            ("link", Some("https://x.org"))
        );
        assert_eq!(plain(&b[2..4]), "one\ntwo");
    }

    #[test]
    fn hostile_input_stays_text() {
        let b = parse(
            "[url=javascript:alert(1)]x[/url] [img]http://t/x.png[/img] [unknown]keep[/unknown] [ unclosed",
        );
        let text = plain(&b);
        assert!(b[0].spans.iter().all(|s| s.link.is_none()));
        assert!(text.contains("[unknown]keep[/unknown]"));
        assert!(text.contains("[ unclosed"));
        assert!(text.contains("http://t/x.png"));
        let b = parse("[url=https://x.org/é]é[/url]");
        assert_eq!(b[0].spans[0].text, "é");
        let b = parse("[noparse][b]raw[/b][/noparse]");
        assert_eq!(plain(&b), "[b]raw[/b]");
        let long = "a".repeat(20_000);
        assert!(plain(&parse(&long)).chars().count() <= MAX_CHARS + 1);
    }
}
