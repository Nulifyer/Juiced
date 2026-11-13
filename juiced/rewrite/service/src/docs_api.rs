use axum::{
    Json,
    extract::{OriginalUri, Path},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

/// The API reference ships inside the binary so it always describes the running build.
const PAGES: &[(&str, &str)] = &[
    ("api/README", include_str!("../../../../docs/api/README.md")),
    (
        "api/anthropic",
        include_str!("../../../../docs/api/anthropic.md"),
    ),
    (
        "api/automatic1111",
        include_str!("../../../../docs/api/automatic1111.md"),
    ),
    ("api/juiced", include_str!("../../../../docs/api/juiced.md")),
    (
        "api/lemonade",
        include_str!("../../../../docs/api/lemonade.md"),
    ),
    (
        "api/llamacpp",
        include_str!("../../../../docs/api/llamacpp.md"),
    ),
    ("api/mcp", include_str!("../../../../docs/api/mcp.md")),
    ("api/ollama", include_str!("../../../../docs/api/ollama.md")),
    ("api/openai", include_str!("../../../../docs/api/openai.md")),
];

fn title<'a>(id: &'a str, content: &'a str) -> &'a str {
    content
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim_end)
        .filter(|title| !title.is_empty())
        .unwrap_or(id)
}

pub fn pages() -> Vec<Value> {
    PAGES
        .iter()
        .map(|(id, content)| json!({"id":id,"title":title(id, content),"bytes":content.len()}))
        .collect()
}

pub fn page(id: &str) -> Option<&'static str> {
    let id = id.strip_suffix(".md").unwrap_or(id);
    PAGES
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, content)| *content)
}

/// Each URL keeps the prefix the caller used, as native v0.9.2 did.
pub(crate) async fn index(OriginalUri(uri): OriginalUri) -> Json<Value> {
    let prefix = uri.path().strip_suffix("/docs").unwrap_or("/v1");
    let docs: Vec<Value> = pages()
        .into_iter()
        .map(|mut entry| {
            entry["url"] = json!(format!("{prefix}/docs/{}", entry["id"].as_str().unwrap()));
            entry
        })
        .collect();
    Json(json!({"version":crate::VERSION,"format":"text/markdown","docs":docs}))
}

pub(crate) async fn document(Path(id): Path<String>) -> Response {
    match page(&id) {
        Some(content) => (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            content,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Documentation page not found"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_pages_have_titles_and_resolve_with_or_without_suffix() {
        let pages = pages();
        assert_eq!(pages.len(), PAGES.len());
        assert!(pages.iter().all(|page| page["bytes"].as_u64() > Some(0)));
        assert!(page("api/openai").is_some());
        assert_eq!(page("api/openai.md"), page("api/openai"));
        assert!(page("../Cargo.toml").is_none());
        assert!(page("api/missing").is_none());
    }
}
