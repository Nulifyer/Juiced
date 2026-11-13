//! Bundled documentation for `juiced_docs`: the API reference served at `/v1/docs` plus the user
//! guide, both compiled into this build so answers match the running version.

use serde_json::{Value, json};

const GUIDE: &[(&str, &str, &str)] = &[
    (
        "guide/README",
        "Dashboard",
        include_str!("../../../../../docs/guide/README.md"),
    ),
    (
        "guide/models",
        "Models > Discover",
        include_str!("../../../../../docs/guide/models.md"),
    ),
    (
        "guide/variants",
        "Models > Discover",
        include_str!("../../../../../docs/guide/variants.md"),
    ),
    (
        "guide/settings",
        "Models > Installed, model inspector",
        include_str!("../../../../../docs/guide/settings.md"),
    ),
    (
        "guide/loading-policy",
        "Loading policy",
        include_str!("../../../../../docs/guide/loading-policy.md"),
    ),
    (
        "guide/runtimes",
        "Engines & hardware",
        include_str!("../../../../../docs/guide/runtimes.md"),
    ),
    (
        "guide/cache",
        "Cache",
        include_str!("../../../../../docs/guide/cache.md"),
    ),
    (
        "guide/testing",
        "Test services",
        include_str!("../../../../../docs/guide/testing.md"),
    ),
    (
        "guide/diagnostics",
        "Diagnostics",
        include_str!("../../../../../docs/guide/diagnostics.md"),
    ),
    (
        "guide/troubleshooting",
        "Diagnostics",
        include_str!("../../../../../docs/guide/troubleshooting.md"),
    ),
    (
        "guide/access",
        "Connect",
        include_str!("../../../../../docs/guide/access.md"),
    ),
    (
        "guide/maintenance",
        "Diagnostics",
        include_str!("../../../../../docs/guide/maintenance.md"),
    ),
    (
        "guide/preparation",
        "Models > Installed",
        include_str!("../../../../../docs/guide/preparation.md"),
    ),
    (
        "guide/barista",
        "Barista (top bar)",
        include_str!("../../../../../docs/guide/barista.md"),
    ),
    (
        "guide/cli",
        "Terminal (lemonade CLI)",
        include_str!("../../../../../docs/guide/cli.md"),
    ),
];

pub(super) const PAGE_BYTES: usize = 6000;
const SECTION_BYTES: usize = 1200;
const MAXIMUM_MATCHES: usize = 5;

struct Page {
    id: &'static str,
    console: Option<&'static str>,
    content: &'static str,
}

fn pages() -> Vec<Page> {
    let mut pages: Vec<Page> = GUIDE
        .iter()
        .map(|(id, console, content)| Page {
            id,
            console: Some(console),
            content,
        })
        .collect();
    for id in PAGES_API {
        if let Some(content) = crate::docs_api::page(id) {
            pages.push(Page {
                id,
                console: Some("Connect"),
                content,
            });
        }
    }
    pages
}

const PAGES_API: &[&str] = &[
    "api/README",
    "api/anthropic",
    "api/automatic1111",
    "api/juiced",
    "api/lemonade",
    "api/llamacpp",
    "api/mcp",
    "api/ollama",
    "api/openai",
];

fn title(content: &str) -> &str {
    content
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .unwrap_or("")
        .trim()
}

fn link(id: &str) -> String {
    match id.strip_prefix("api/") {
        Some(_) => format!("/v1/docs/{id}"),
        None => format!("docs/{id}.md"),
    }
}

pub(super) fn index() -> Vec<Value> {
    pages()
        .iter()
        .map(|page| json!({"page":page.id,"title":title(page.content),"console":page.console,"link":link(page.id),"bytes":page.content.len()}))
        .collect()
}

fn clip(text: &str, maximum: usize) -> (&str, bool) {
    if text.len() <= maximum {
        return (text, false);
    }
    let mut end = maximum;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// One page, paged by byte offset so long references stay within the tool output bound.
pub(super) fn read(id: &str, offset: usize) -> Option<Value> {
    let id = id.strip_suffix(".md").unwrap_or(id);
    let pages = pages();
    let page = pages.iter().find(|page| page.id == id)?;
    let mut start = offset.min(page.content.len());
    while !page.content.is_char_boundary(start) {
        start -= 1;
    }
    let (text, truncated) = clip(&page.content[start..], PAGE_BYTES);
    Some(json!({
        "page": page.id, "title": title(page.content), "console": page.console, "link": link(page.id),
        "offset": start, "text": text,
        "next_offset": truncated.then_some(start + text.len()),
    }))
}

/// Sections whose text contains every query word, best first.
pub(super) fn search(query: &str) -> Vec<Value> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|word| word.to_lowercase())
        .filter(|word| word.len() > 1)
        .take(8)
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(usize, Value)> = Vec::new();
    for page in pages() {
        let mut heading = title(page.content);
        let mut section = String::new();
        let flush = |heading: &str, section: &str, hits: &mut Vec<(usize, Value)>| {
            let lower = section.to_lowercase();
            if section.trim().is_empty() || !words.iter().all(|word| lower.contains(word.as_str()))
            {
                return;
            }
            let score = words
                .iter()
                .map(|word| lower.matches(word.as_str()).count())
                .sum::<usize>()
                + if words
                    .iter()
                    .any(|word| heading.to_lowercase().contains(word.as_str()))
                {
                    10
                } else {
                    0
                };
            let (text, truncated) = clip(section.trim(), SECTION_BYTES);
            hits.push((
                score,
                json!({"page":page.id,"section":heading,"console":page.console,"link":link(page.id),"text":text,"truncated":truncated}),
            ));
        };
        for line in page.content.lines() {
            if let Some(next) = line.strip_prefix("## ").or_else(|| line.strip_prefix("# ")) {
                flush(heading, &section, &mut hits);
                section.clear();
                heading = next.trim();
            }
            section.push_str(line);
            section.push('\n');
        }
        flush(heading, &section, &mut hits);
    }
    hits.sort_by(|left, right| right.0.cmp(&left.0));
    hits.into_iter()
        .take(MAXIMUM_MATCHES)
        .map(|(_, hit)| hit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_and_api_pages_are_bundled_and_searchable() {
        let index = index();
        assert!(index.iter().any(|page| page["page"] == "guide/cache"));
        assert!(index.iter().any(|page| page["page"] == "api/openai"));
        assert!(index.iter().all(|page| page["page"] != "api"));
        let page = read("guide/cache.md", 0).unwrap();
        assert_eq!(page["console"], "Cache");
        assert!(page["text"].as_str().unwrap().len() <= PAGE_BYTES);
        assert!(read("../Cargo.toml", 0).is_none());
        let hits = search("API key");
        assert!(!hits.is_empty());
        assert!(hits.len() <= MAXIMUM_MATCHES);
        assert!(search("zzzz-no-such-words").is_empty());
        assert!(search(" ").is_empty());
    }
}
