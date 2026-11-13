//! What a client needs to register a model: per-request context, output limit and the chat
//! features its template supports. Every value names its evidence; nothing is guessed.

use serde_json::Value;

/// Version of the discovery fields on catalog, model list and capability entries.
pub const SCHEMA_VERSION: u32 = 1;

/// Strings a template uses to render reasoning content. Rendering it does not show that the
/// model reasons: many templates only carry earlier reasoning through the history.
const REASONING_MARKERS: [&str; 10] = [
    "enable_thinking",
    "<think>",
    "</think>",
    "reasoning_content",
    "<|channel|>",
    "[THINK]",
    "<|START_THINKING|>",
    "<seed:think>",
    "reasoning_effort",
    "thinking_budget",
];

/// Opening tags that, written after `add_generation_prompt`, start the reply in reasoning.
const REASONING_OPENERS: [&str; 4] = ["<think>", "[THINK]", "<|START_THINKING|>", "<seed:think>"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateFacts {
    /// The template has a reasoning switch or starts the reply in reasoning.
    pub thinking: bool,
    /// The template renders reasoning content somewhere.
    pub renders_reasoning: bool,
    /// The generation prompt opens a reasoning block.
    pub opens_reasoning: bool,
    pub thinking_control: Option<&'static str>,
    pub tools: bool,
}

pub fn template_facts(template: &str) -> TemplateFacts {
    let renders_reasoning = REASONING_MARKERS
        .iter()
        .any(|marker| template.contains(marker));
    let opens_reasoning = template
        .rfind("add_generation_prompt")
        .map(|start| &template[start..])
        .is_some_and(|prompt| REASONING_OPENERS.iter().any(|tag| prompt.contains(tag)));
    let thinking_control = if template.contains("enable_thinking") {
        Some("chat_template_kwargs.enable_thinking")
    } else if template.contains("reasoning_effort") {
        Some("reasoning_effort")
    } else {
        None
    };
    TemplateFacts {
        thinking: thinking_control.is_some()
            || opens_reasoning
            || template.contains("thinking_budget"),
        renders_reasoning,
        opens_reasoning,
        thinking_control,
        tools: template.contains("tools") && template.contains("tool_call"),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchFacts {
    pub context: Option<u64>,
    /// `None` means llama-server chooses: four slots sharing one unified cache.
    pub slots: Option<u64>,
    pub kv_unified: Option<bool>,
    pub kv_per_slot: Option<u64>,
    pub predict: Option<u64>,
    pub template_override: bool,
    pub inline_template: Option<String>,
    pub jinja: bool,
    pub thinking: Option<&'static str>,
}

const VALUE_FLAGS: [&str; 16] = [
    "-c",
    "--ctx-size",
    "-np",
    "--parallel",
    "--kv-unified-per-slot",
    "-n",
    "--predict",
    "--n-predict",
    "--chat-template",
    "--chat-template-file",
    "-rea",
    "--reasoning",
    "--reasoning-budget",
    "--chat-template-kwargs",
    "--reasoning-format",
    "--model",
];

fn positive(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|value| *value > 0)
}

impl LaunchFacts {
    pub fn read(args: &[String]) -> Self {
        let mut facts = Self {
            jinja: true,
            ..Self::default()
        };
        let mut budget_off = false;
        let mut kwargs_thinking = None;
        let mut position = 0;
        while position < args.len() {
            let (flag, inline) = args[position]
                .split_once('=')
                .map_or((args[position].as_str(), None), |(flag, value)| {
                    (flag, Some(value))
                });
            if !VALUE_FLAGS.contains(&flag) {
                match flag {
                    "-kvu" | "--kv-unified" => facts.kv_unified = Some(true),
                    "-no-kvu" | "--no-kv-unified" => facts.kv_unified = Some(false),
                    "--jinja" => facts.jinja = true,
                    "--no-jinja" => facts.jinja = false,
                    _ => {}
                }
                position += 1;
                continue;
            }
            let value = inline.or_else(|| args.get(position + 1).map(String::as_str));
            position += if inline.is_some() { 1 } else { 2 };
            let Some(value) = value else { break };
            match flag {
                "-c" | "--ctx-size" => facts.context = positive(value),
                "-np" | "--parallel" => facts.slots = positive(value),
                "--kv-unified-per-slot" => facts.kv_per_slot = positive(value),
                "-n" | "--predict" | "--n-predict" => facts.predict = positive(value),
                "--chat-template" => {
                    facts.template_override = true;
                    facts.inline_template =
                        (value.contains("{%") || value.contains("{{")).then(|| value.to_owned());
                }
                "--chat-template-file" => {
                    facts.template_override = true;
                    facts.inline_template = None;
                }
                "-rea" | "--reasoning" => {
                    facts.thinking = match value {
                        "on" | "true" | "1" | "enabled" => Some("on"),
                        "off" | "false" | "0" | "disabled" => Some("off"),
                        _ => None,
                    }
                }
                "--reasoning-budget" => budget_off = value == "0",
                "--chat-template-kwargs" => {
                    kwargs_thinking = serde_json::from_str::<Value>(value)
                        .ok()
                        .and_then(|kwargs| kwargs.get("enable_thinking").and_then(Value::as_bool))
                        .map(|enabled| if enabled { "on" } else { "off" });
                }
                _ => {}
            }
        }
        facts.thinking = facts
            .thinking
            .or(budget_off.then_some("off"))
            .or(kwargs_thinking);
        facts
    }
}

fn pad(tokens: u64) -> u64 {
    tokens.div_ceil(256).saturating_mul(256)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPlan {
    pub slots: u64,
    pub unified: bool,
    pub per_request: Option<u64>,
    /// The engine sizes the context itself (`--fit` may shrink it below the training size).
    pub automatic: bool,
}

/// Mirrors llama-server: the total is padded to 256 tokens, then split between slots unless the
/// cache is unified, then capped by `--kv-unified-per-slot` and the training context.
pub fn context_plan(launch: &LaunchFacts, trained: Option<u64>) -> ContextPlan {
    let slots = launch.slots.unwrap_or(4);
    let unified = launch.kv_unified.unwrap_or(launch.slots.is_none());
    let (total, automatic) = match (launch.context, launch.kv_per_slot) {
        (Some(context), _) => (Some(context), false),
        (None, Some(per_slot)) => (Some(per_slot.saturating_mul(slots)), false),
        (None, None) => (trained, true),
    };
    let per_request = total.map(|total| {
        let padded = pad(total);
        let sequence = if unified {
            padded
        } else {
            pad(padded / slots).max(256)
        };
        let capped = launch
            .kv_per_slot
            .map_or(sequence, |limit| sequence.min(limit));
        trained.map_or(capped, |trained| capped.min(trained))
    });
    ContextPlan {
        slots,
        unified,
        per_request,
        automatic,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineFacts {
    pub per_request: Option<u64>,
    pub slots: Option<u64>,
    pub template: Option<TemplateFacts>,
    pub tools: Option<bool>,
    pub vision: Option<bool>,
    /// Effort values the template renders distinctly, from `chat_template_caps`.
    pub effort_levels: Option<Vec<String>>,
}

/// Reads the llama-server `/props` fields that describe one request: the slot context
/// (`default_generation_settings.n_ctx`), the effective chat template and its rendered caps.
pub fn engine_facts(props: &Value) -> EngineFacts {
    let number = |value: Option<&Value>| value.and_then(Value::as_u64).filter(|value| *value > 0);
    let template = props
        .get("chat_template")
        .and_then(Value::as_str)
        .filter(|template| !template.is_empty())
        .map(template_facts);
    let caps = props.get("chat_template_caps");
    let tools = caps.and_then(|caps| {
        Some(caps.get("supports_tools")?.as_bool()? && caps.get("supports_tool_calls")?.as_bool()?)
    });
    EngineFacts {
        per_request: number(props.pointer("/default_generation_settings/n_ctx")),
        slots: number(props.get("total_slots")),
        tools: tools.or(template.as_ref().map(|facts| facts.tools)),
        template,
        vision: props.pointer("/modalities/vision").and_then(Value::as_bool),
        effort_levels: caps
            .and_then(|caps| caps.get("reasoning_effort_levels"))
            .and_then(Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|levels| !levels.is_empty()),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Native clients send `enable_thinking` or `thinking` at the top level; llama-server reads only
/// `chat_template_kwargs.enable_thinking`. A value already in `chat_template_kwargs` wins.
pub fn normalize_thinking(body: &[u8]) -> Option<Vec<u8>> {
    if !contains(body, b"thinking\"") {
        return None;
    }
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object_mut()?;
    if !object.contains_key("enable_thinking") && !object.contains_key("thinking") {
        return None;
    }
    let requested = |value: Option<&Value>| match value? {
        Value::Bool(enabled) => Some(*enabled),
        Value::Object(thinking) => match thinking.get("type")?.as_str()? {
            "enabled" => Some(true),
            "disabled" => Some(false),
            _ => None,
        },
        _ => None,
    };
    let enabled = requested(object.get("enable_thinking")).or(requested(object.get("thinking")));
    object.remove("enable_thinking");
    object.remove("thinking");
    if let Some(enabled) = enabled {
        let kwargs = object
            .entry("chat_template_kwargs")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(kwargs) = kwargs.as_object_mut() {
            kwargs
                .entry("enable_thinking")
                .or_insert(Value::Bool(enabled));
        }
    }
    serde_json::to_vec(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn per_request_context_follows_llama_server_slot_arithmetic() {
        let plan = |values: &[&str], trained| {
            context_plan(&LaunchFacts::read(&args(values)), trained).per_request
        };
        assert_eq!(
            plan(&["--ctx-size", "262144", "--parallel", "2"], Some(262144)),
            Some(131072)
        );
        assert_eq!(plan(&["-c", "1000", "-np", "3"], Some(4096)), Some(512));
        assert_eq!(
            plan(&["--ctx-size=256", "--parallel", "1"], Some(128)),
            Some(128)
        );
        assert_eq!(plan(&["--ctx-size", "8192"], Some(32768)), Some(8192));
        assert_eq!(
            plan(
                &["--ctx-size", "8192", "--parallel", "4", "--kv-unified"],
                None
            ),
            Some(8192)
        );
        assert_eq!(
            plan(
                &[
                    "--parallel",
                    "2",
                    "--kv-unified",
                    "--kv-unified-per-slot",
                    "3000"
                ],
                Some(32768)
            ),
            Some(3000)
        );
        let automatic = context_plan(&LaunchFacts::read(&args(&["--parallel", "2"])), Some(4096));
        assert!(automatic.automatic);
        assert_eq!(automatic.per_request, Some(2048));
        assert_eq!(plan(&["--parallel", "2"], None), None);
    }

    #[test]
    fn launch_thinking_and_template_overrides_are_read() {
        let facts = LaunchFacts::read(&args(&[
            "--chat-template-kwargs",
            r#"{"enable_thinking":false}"#,
            "--predict",
            "4096",
            "--no-jinja",
        ]));
        assert_eq!(facts.thinking, Some("off"));
        assert_eq!(facts.predict, Some(4096));
        assert!(!facts.jinja);
        assert_eq!(
            LaunchFacts::read(&args(&["--reasoning", "on"])).thinking,
            Some("on")
        );
        assert_eq!(
            LaunchFacts::read(&args(&["--reasoning-budget", "0"])).thinking,
            Some("off")
        );
        let inline = LaunchFacts::read(&args(&[
            "--chat-template",
            "{% for m in messages %}{% endfor %}",
        ]));
        assert!(inline.template_override && inline.inline_template.is_some());
        let named = LaunchFacts::read(&args(&["--chat-template", "chatml"]));
        assert!(named.template_override && named.inline_template.is_none());
    }

    #[test]
    fn template_markers_name_thinking_and_tools() {
        let qwen = "{%- if tools %}{{ tool_call }}{% endif %}{%- if enable_thinking is defined and enable_thinking is false %}<think>\n\n</think>{% endif %}";
        let facts = template_facts(qwen);
        assert!(facts.thinking && facts.renders_reasoning && facts.tools);
        assert_eq!(
            facts.thinking_control,
            Some("chat_template_kwargs.enable_thinking")
        );
        let plain = "{% for message in messages %}<|im_start|>{{ message.role }}{% endfor %}";
        assert_eq!(
            template_facts(plain),
            TemplateFacts {
                thinking: false,
                renders_reasoning: false,
                opens_reasoning: false,
                thinking_control: None,
                tools: false
            }
        );
        assert_eq!(
            template_facts("{{ reasoning_effort }}<|channel|>analysis").thinking_control,
            Some("reasoning_effort")
        );
    }

    #[test]
    fn rendering_reasoning_is_not_evidence_that_the_model_reasons() {
        let history = "{% for m in messages %}{% if m.reasoning_content %}[THINK]{{ m.reasoning_content }}[/THINK]{% endif %}{{ m.content }}{% endfor %}{% if add_generation_prompt %}[INST]{% endif %}";
        let facts = template_facts(history);
        assert!(facts.renders_reasoning && !facts.opens_reasoning && !facts.thinking);
        assert_eq!(facts.thinking_control, None);
        let opens = "{% for m in messages %}{{ m.content.split('</think>')[-1] }}{% endfor %}{% if add_generation_prompt %}<|Assistant|><think>\n{% endif %}";
        let facts = template_facts(opens);
        assert!(facts.opens_reasoning && facts.thinking);
        assert_eq!(facts.thinking_control, None);
    }

    #[test]
    fn engine_caps_carry_reasoning_effort_levels() {
        let facts = engine_facts(
            &json!({"chat_template_caps": {"supports_tools": true, "supports_tool_calls": true, "reasoning_effort_levels": ["low", "medium", "high"]}}),
        );
        assert_eq!(
            facts.effort_levels,
            Some(vec!["low".into(), "medium".into(), "high".into()])
        );
        let none = engine_facts(&json!({"chat_template_caps": {"reasoning_effort_levels": []}}));
        assert_eq!(none.effort_levels, None);
    }

    #[test]
    fn native_thinking_fields_become_template_kwargs() {
        let normalized = |body: Value| {
            normalize_thinking(body.to_string().as_bytes())
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap())
        };
        assert_eq!(
            normalized(json!({"model":"m","enable_thinking":false,"messages":[]})).unwrap(),
            json!({"model":"m","messages":[],"chat_template_kwargs":{"enable_thinking":false}})
        );
        assert_eq!(
            normalized(json!({"model":"m","thinking":{"type":"enabled","budget_tokens":10}}))
                .unwrap()["chat_template_kwargs"],
            json!({"enable_thinking":true})
        );
        assert_eq!(
            normalized(json!({"enable_thinking":true,"thinking":false,"chat_template_kwargs":{"enable_thinking":false,"x":1}}))
                .unwrap()["chat_template_kwargs"],
            json!({"enable_thinking":false,"x":1})
        );
        assert_eq!(
            normalized(json!({"messages":[{"role":"assistant","thinking":"kept"}]})),
            None
        );
        assert_eq!(
            normalized(json!({"chat_template_kwargs":{"enable_thinking":true}})),
            None
        );
    }

    #[test]
    fn engine_props_prefer_rendered_caps() {
        let facts = engine_facts(&json!({
            "default_generation_settings":{"n_ctx":4096},"total_slots":2,
            "chat_template":"{% if tools %}{{ tool_calls }}{% endif %}",
            "chat_template_caps":{"supports_tools":false,"supports_tool_calls":true},
            "modalities":{"vision":true}
        }));
        assert_eq!(facts.per_request, Some(4096));
        assert_eq!(facts.slots, Some(2));
        assert_eq!(facts.tools, Some(false));
        assert_eq!(facts.vision, Some(true));
        assert_eq!(engine_facts(&json!({})), EngineFacts::default());
    }
}
