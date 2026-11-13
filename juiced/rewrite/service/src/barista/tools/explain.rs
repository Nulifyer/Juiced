use super::*;

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "juiced_help",
        group: Group::Explain,
        title: "Procedure",
        description: "A shipped step-by-step procedure for a whole job: the tools to use in order, how to verify and how to recover. Omit topic to list them.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "topic":{"type":"string","enum":super::super::skills::topics()}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_docs",
        group: Group::Explain,
        title: "Docs",
        description: "Search or read the docs bundled with this version: the user guide and the API reference, with the console location for each page.",
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","properties":{
                "query":{"type":"string","maxLength":100,"description":"Two to four words."},
                "page":{"type":"string","maxLength":64,"description":"Page id such as guide/cache."},
                "offset":{"type":"integer","minimum":0}
            },"additionalProperties":false})
        },
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelpArgs {
    topic: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocsArgs {
    query: Option<String>,
    page: Option<String>,
    offset: Option<usize>,
}

pub(super) async fn run(_: &Context<'_>, name: &str, args: Value) -> Option<Result<Outcome>> {
    Some(match name {
        "juiced_help" => help(args),
        "juiced_docs" => docs(args),
        _ => return None,
    })
}

fn help(args: Value) -> Result<Outcome> {
    let HelpArgs { topic } = parse(args)?;
    let Some(topic) = topic else {
        return Ok(Outcome::new(
            "Procedures shipped with this version. Call juiced_help with a topic.",
            json!({"topics": super::super::skills::index()}),
        ));
    };
    let skill = super::super::skills::find(&topic).ok_or_else(|| {
        Failure::new(
            "unknown_topic",
            format!(
                "No procedure is named {topic}. Topics: {}.",
                super::super::skills::topics().join(", ")
            ),
            404,
        )
    })?;
    Ok(Outcome::new(
        format!(
            "Procedure {} v{}: {}",
            skill.topic, skill.version, skill.title
        ),
        json!({"topic": skill.topic, "version": skill.version, "procedure": skill.body}),
    ))
}

fn docs(args: Value) -> Result<Outcome> {
    let docs: DocsArgs = parse(args)?;
    match (docs.page, docs.query) {
        (Some(page), _) => {
            bounded(&page, 1, 64, "page")?;
            let found = super::super::docs::read(&page, docs.offset.unwrap_or(0)).ok_or_else(|| {
                Failure::new("unknown_page", format!("No bundled page is named {page}. Search with query, or omit both to list pages."), 404)
            })?;
            let mut outcome = Outcome::new(
                format!(
                    "{} ({}).",
                    found["title"].as_str().unwrap_or(&page),
                    found["link"].as_str().unwrap_or("")
                ),
                found.clone(),
            );
            if found["next_offset"].is_u64() {
                outcome = outcome.next(&["juiced_docs"]);
            }
            Ok(outcome)
        }
        (None, Some(query)) => {
            bounded(&query, 1, 100, "query")?;
            let hits = super::super::docs::search(&query);
            Ok(Outcome::new(
                if hits.is_empty() {
                    format!(
                        "No bundled docs match \"{query}\". Try other words, or list pages without a query."
                    )
                } else {
                    format!("{} section(s) match \"{query}\".", hits.len())
                },
                json!({"matches": hits}),
            ))
        }
        (None, None) => Ok(Outcome::new(
            "Bundled docs for this version.",
            json!({"pages": super::super::docs::index()}),
        )),
    }
}
