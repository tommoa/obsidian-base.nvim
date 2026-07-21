//! Markdown metadata extraction for tags and Obsidian-style wikilinks.

use std::collections::HashSet;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};

#[derive(Debug, Eq, PartialEq)]
/// Tags and link targets discovered in prose within a Markdown document.
pub struct MarkdownMetadata {
    /// Unique normalized tags encountered in document order.
    pub tags: Vec<String>,
    /// Wikilink targets encountered in document order.
    pub links: Vec<String>,
}

pub fn markdown_metadata(markdown: &str) -> MarkdownMetadata {
    let mut tags = Vec::new();
    let mut seen_tags = HashSet::new();
    let mut links = Vec::new();
    let mut code_depth = 0usize;
    let mut html_depth = 0usize;
    for event in Parser::new_ext(markdown, Options::ENABLE_WIKILINKS) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_depth += 1,
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) if code_depth == 0 && html_depth == 0 => {
                let target = dest_url.split('#').next().unwrap_or_default().trim();
                if !target.is_empty() {
                    links.push(target.to_owned());
                }
            }
            Event::End(TagEnd::CodeBlock) => code_depth = code_depth.saturating_sub(1),
            Event::InlineHtml(html) if html.trim_start().starts_with("</") => {
                html_depth = html_depth.saturating_sub(1);
            }
            Event::InlineHtml(html) => {
                let html = html.trim();
                if !html.ends_with("/>") && !is_void_html(html) {
                    html_depth += 1;
                }
            }
            Event::Text(text) if code_depth == 0 && html_depth == 0 => {
                scan_text(&text, &mut tags, &mut seen_tags, &mut links);
            }
            _ => {}
        }
    }
    MarkdownMetadata { tags, links }
}

fn is_void_html(html: &str) -> bool {
    [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ]
    .iter()
    .any(|tag| html.get(1..).is_some_and(|rest| rest.starts_with(tag)))
}

fn scan_text(
    text: &str,
    tags: &mut Vec<String>,
    seen_tags: &mut HashSet<String>,
    links: &mut Vec<String>,
) {
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    for (position, (start, character)) in characters.iter().enumerate() {
        if *character != '#' || (position > 0 && !characters[position - 1].1.is_whitespace()) {
            continue;
        }
        let mut end = start + 1;
        for (_, character) in characters.iter().skip(position + 1) {
            if character.is_alphanumeric() || matches!(character, '_' | '/' | '-') {
                end += character.len_utf8();
            } else {
                break;
            }
        }
        if end > start + 1 {
            let tag = text[*start..end].to_owned();
            if seen_tags.insert(tag.clone()) {
                tags.push(tag);
            }
        }
    }
    let mut remaining = text;
    while let Some(open) = remaining.find("[[") {
        remaining = &remaining[open + 2..];
        let Some(close) = remaining.find("]]") else {
            break;
        };
        let reference = &remaining[..close];
        let target = reference
            .split('|')
            .next()
            .unwrap_or_default()
            .split('#')
            .next()
            .unwrap_or_default()
            .trim();
        if !target.is_empty() {
            links.push(target.to_owned());
        }
        remaining = &remaining[close + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_prose_but_not_code_or_html() {
        let metadata = markdown_metadata(
            "# heading #timed [[A Note#section|Alias]]\n`#code [[No]]`\n```txt\n#hidden [[Nope]]\n```\n<span>#html [[No]]</span>",
        );
        assert_eq!(metadata.tags, ["#timed"]);
        assert_eq!(metadata.links, ["A Note"]);
    }
}
