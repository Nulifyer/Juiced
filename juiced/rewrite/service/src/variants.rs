use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub key: String,
    pub label: String,
    pub quant: Option<String>,
}

const BUILD_SUFFIXES: [&str; 8] = [
    "gguf", "hybrid", "vulkan", "rocm", "cpu", "npu", "cuda", "metal",
];

fn quant_token(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "BF16" | "F16" | "F32" | "FP16" | "FP8" | "MXFP4" | "INT4" | "INT8"
    ) {
        return true;
    }
    let rest = upper
        .strip_prefix("IQ")
        .or_else(|| upper.strip_prefix('Q'))
        .unwrap_or("");
    let mut parts = rest.split('_');
    parts
        .next()
        .is_some_and(|bits| !bits.is_empty() && bits.bytes().all(|byte| byte.is_ascii_digit()))
        && parts
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

fn without_publisher(base: &str) -> &str {
    // Re-uploaders name files `Publisher_Model-Size-...`; a bare `A_B` name has no size part.
    match base.split_once('_') {
        Some((_, rest)) if rest.contains('-') && !rest.starts_with('-') => rest,
        _ => base,
    }
}

/// Splits a registered name into the base model it was quantized from and its quant label.
pub fn classify(name: &str) -> Variant {
    let name = name.strip_prefix("builtin.").unwrap_or(name);
    let tokens: Vec<&str> = name.split('-').collect();
    let quant_at = tokens.iter().position(|token| quant_token(token));
    let (mut base, quant) = match quant_at {
        Some(index) => {
            let start = if index > 0 && tokens[index - 1].eq_ignore_ascii_case("ud") {
                index - 1
            } else {
                index
            };
            (
                tokens[..start].to_vec(),
                Some(tokens[start..=index].join("-")),
            )
        }
        None => (tokens.clone(), None),
    };
    if quant_at.is_none() {
        while base.len() > 1
            && base
                .last()
                .is_some_and(|token| BUILD_SUFFIXES.contains(&token.to_ascii_lowercase().as_str()))
        {
            base.pop();
        }
    }
    base.retain(|token| !token.eq_ignore_ascii_case("gguf"));
    let joined = if base.is_empty() {
        name.to_owned()
    } else {
        base.join("-")
    };
    let label = without_publisher(&joined).to_owned();
    Variant {
        key: label.to_ascii_lowercase(),
        label,
        quant,
    }
}

/// Reads the Hugging Face repository name from a `models--owner--name` cache directory.
pub fn repository_name(path: &Path) -> Option<String> {
    path.components().find_map(|component| {
        let text = component.as_os_str().to_str()?;
        let rest = text.strip_prefix("models--")?;
        let (_, name) = rest.split_once("--")?;
        (!name.is_empty()).then(|| name.to_owned())
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub label: String,
    pub members: Vec<String>,
}

/// Groups registrations whose names or source repositories share a base model.
pub fn groups<'a>(models: impl IntoIterator<Item = (&'a str, &'a Path)>) -> Vec<Group> {
    let models: Vec<(&str, Variant, Option<Variant>)> = models
        .into_iter()
        .map(|(id, path)| {
            (
                id,
                classify(id),
                repository_name(path).map(|name| classify(&name)),
            )
        })
        .collect();
    let mut parent: Vec<usize> = (0..models.len()).collect();
    fn root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    let mut owners: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, (_, name, repository)) in models.iter().enumerate() {
        for key in std::iter::once(&name.key).chain(repository.as_ref().map(|variant| &variant.key))
        {
            match owners.get(key.as_str()) {
                Some(&other) => {
                    let (left, right) = (root(&mut parent, index), root(&mut parent, other));
                    parent[left.max(right)] = left.min(right);
                }
                None => {
                    owners.insert(key, index);
                }
            }
        }
    }
    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..models.len() {
        let owner = root(&mut parent, index);
        components.entry(owner).or_default().push(index);
    }
    let mut result: Vec<Group> = components
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|members| {
            let mut counts: BTreeMap<&str, (usize, &Variant)> = BTreeMap::new();
            for &index in &members {
                let (_, name, repository) = &models[index];
                for variant in std::iter::once(name).chain(repository.as_ref()) {
                    counts.entry(variant.key.as_str()).or_insert((0, variant)).0 += 1;
                }
            }
            let (_, named) = counts
                .into_values()
                .rev()
                .max_by_key(|(count, _)| *count)
                .expect("non-empty group");
            let mut ids: Vec<String> = members
                .iter()
                .map(|&index| models[index].0.to_owned())
                .collect();
            ids.sort();
            Group {
                id: named.key.clone(),
                label: named.label.clone(),
                members: ids,
            }
        })
        .collect();
    result.sort_by(|left, right| left.id.cmp(&right.id));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const LIVE: [&str; 43] = [
        "Anima-Turbo-v1.1",
        "Chroma1-Flash-GGUF-Q8_0",
        "FLUX.2-klein-4B-GGUF-Q4_0",
        "Flux-2-Klein-9B-GGUF",
        "Mellum2.1-12B-A2.5B-Thinking-GGUF-Q4_K_M",
        "Mellum2.1-12B-A2.5B-Thinking-GGUF-Q6_K",
        "Mellum2.1-12B-A2.5B-Thinking-GGUF-Q8_0",
        "Nemotron-3-Nano-30B-A3B-GGUF-Q4_K_M",
        "Ornith-1.5-35B-A3B-GGUF",
        "Ornith-1.5-35B-A3B-GGUF-Q6_K",
        "Qwen-Image-2512-GGUF",
        "Qwen3-0.6B-GGUF",
        "Qwen3-4B-Instruct-2507-GGUF",
        "Qwen3-Coder-Next-GGUF-Q4_K_M",
        "Qwen3.5-2B",
        "Qwen3.5-2B-GGUF-Q6_K_XL",
        "Qwen3.5-4B-Uncensored-HauhauCS-Aggressive-Q8_0",
        "Qwen3.6-27B-MTP-GGUF",
        "Qwen3.6-35B-A3B-GGUF-Q8_0",
        "Qwen3.6-35B-A3B-GGUF-UD-Q3_K_M",
        "Qwen3.6-35B-A3B-GGUF-UD-Q4_K_M",
        "Qwen3.6-35B-A3B-GGUF-UD-Q6_K",
        "Qwen3.8-27B-GGUF",
        "Qwen3.8-27B-GGUF-UD-Q4_K_XL-unsloth",
        "Qwen3.8-27B-GGUF-UD-Q6_K_XL",
        "Qwen3.8-27B-GGUF-UD-Q8_K_XL",
        "Qwen3.8-35B-A3B-Distill-GGUF-Q4_K_M",
        "Qwopus3.6-35B-A3B-Coder-MTP-GGUF-Q4_K_M",
        "RealVisXL_V5.0_Lightning",
        "SD-Turbo-GGUF",
        "Skyfall-31B-v4.2-IQ4_NL-Vulkan",
        "TheDrummer_Skyfall-31B-v4.2-GGUF-IQ4_NL",
        "Tiel-Coder-35B-A3B-GGUF-MTP-UD-Q4_K_XL",
        "Z-Image-Engineer-V6-Q8_0",
        "Z-Image-Turbo-GGUF-Q8_0",
        "asr4all-l-gptq-Q8_0",
        "asr4all-m-gptq-Q8_0",
        "asr4all-s-gptq-Q8_0",
        "builtin.Flux-2-Klein-9B-GGUF",
        "builtin.Qwen-Image-2512-GGUF",
        "kokoro-v1",
        "nex-agi_Nex-N2.5-mini-GGUF-Q4_K_M",
        "z-image-turbo-nsfw-v2-GGUF-Q8_0",
    ];

    fn live_groups() -> Vec<Group> {
        let path = PathBuf::from("/data/models/weights.gguf");
        groups(LIVE.iter().map(|id| (*id, path.as_path())))
    }

    #[test]
    fn quant_labels_are_recognized_and_names_are_not() {
        for quant in [
            "Q4_K_M", "Q6_K", "Q8_0", "IQ4_NL", "Q4_K_XL", "Q8_K_XL", "IQ2_XXS", "Q4_0", "BF16",
            "f16",
        ] {
            assert!(quant_token(quant), "{quant}");
        }
        for name in [
            "Qwen3.8", "27B", "A3B", "v4.2", "Q", "QwQ", "Q4_", "MTP", "GGUF", "Q4-K",
        ] {
            assert!(!quant_token(name), "{name}");
        }
    }

    #[test]
    fn live_registrations_classify_to_their_base_model() {
        let cases = [
            ("Qwen3.8-27B-GGUF", "Qwen3.8-27B", None),
            (
                "Qwen3.8-27B-GGUF-UD-Q4_K_XL-unsloth",
                "Qwen3.8-27B",
                Some("UD-Q4_K_XL"),
            ),
            (
                "Qwen3.8-27B-GGUF-UD-Q8_K_XL",
                "Qwen3.8-27B",
                Some("UD-Q8_K_XL"),
            ),
            (
                "Mellum2.1-12B-A2.5B-Thinking-GGUF-Q6_K",
                "Mellum2.1-12B-A2.5B-Thinking",
                Some("Q6_K"),
            ),
            (
                "Skyfall-31B-v4.2-IQ4_NL-Vulkan",
                "Skyfall-31B-v4.2",
                Some("IQ4_NL"),
            ),
            (
                "TheDrummer_Skyfall-31B-v4.2-GGUF-IQ4_NL",
                "Skyfall-31B-v4.2",
                Some("IQ4_NL"),
            ),
            (
                "nex-agi_Nex-N2.5-mini-GGUF-Q4_K_M",
                "Nex-N2.5-mini",
                Some("Q4_K_M"),
            ),
            ("RealVisXL_V5.0_Lightning", "RealVisXL_V5.0_Lightning", None),
            ("builtin.Flux-2-Klein-9B-GGUF", "Flux-2-Klein-9B", None),
            (
                "Tiel-Coder-35B-A3B-GGUF-MTP-UD-Q4_K_XL",
                "Tiel-Coder-35B-A3B-MTP",
                Some("UD-Q4_K_XL"),
            ),
            ("kokoro-v1", "kokoro-v1", None),
        ];
        for (name, label, quant) in cases {
            let variant = classify(name);
            assert_eq!(variant.label, label, "{name}");
            assert_eq!(variant.key, label.to_ascii_lowercase(), "{name}");
            assert_eq!(variant.quant.as_deref(), quant, "{name}");
        }
    }

    #[test]
    fn live_catalog_groups_only_quants_of_the_same_base_model() {
        let found: Vec<(String, Vec<String>)> = live_groups()
            .into_iter()
            .map(|group| (group.label, group.members))
            .collect();
        let expected: Vec<(&str, Vec<&str>)> = vec![
            (
                "Flux-2-Klein-9B",
                vec!["Flux-2-Klein-9B-GGUF", "builtin.Flux-2-Klein-9B-GGUF"],
            ),
            (
                "Mellum2.1-12B-A2.5B-Thinking",
                vec![
                    "Mellum2.1-12B-A2.5B-Thinking-GGUF-Q4_K_M",
                    "Mellum2.1-12B-A2.5B-Thinking-GGUF-Q6_K",
                    "Mellum2.1-12B-A2.5B-Thinking-GGUF-Q8_0",
                ],
            ),
            (
                "Ornith-1.5-35B-A3B",
                vec!["Ornith-1.5-35B-A3B-GGUF", "Ornith-1.5-35B-A3B-GGUF-Q6_K"],
            ),
            (
                "Qwen-Image-2512",
                vec!["Qwen-Image-2512-GGUF", "builtin.Qwen-Image-2512-GGUF"],
            ),
            ("Qwen3.5-2B", vec!["Qwen3.5-2B", "Qwen3.5-2B-GGUF-Q6_K_XL"]),
            (
                "Qwen3.6-35B-A3B",
                vec![
                    "Qwen3.6-35B-A3B-GGUF-Q8_0",
                    "Qwen3.6-35B-A3B-GGUF-UD-Q3_K_M",
                    "Qwen3.6-35B-A3B-GGUF-UD-Q4_K_M",
                    "Qwen3.6-35B-A3B-GGUF-UD-Q6_K",
                ],
            ),
            (
                "Qwen3.8-27B",
                vec![
                    "Qwen3.8-27B-GGUF",
                    "Qwen3.8-27B-GGUF-UD-Q4_K_XL-unsloth",
                    "Qwen3.8-27B-GGUF-UD-Q6_K_XL",
                    "Qwen3.8-27B-GGUF-UD-Q8_K_XL",
                ],
            ),
            (
                "Skyfall-31B-v4.2",
                vec![
                    "Skyfall-31B-v4.2-IQ4_NL-Vulkan",
                    "TheDrummer_Skyfall-31B-v4.2-GGUF-IQ4_NL",
                ],
            ),
        ];
        let expected: Vec<(String, Vec<String>)> = expected
            .into_iter()
            .map(|(label, members)| {
                (
                    label.to_owned(),
                    members.into_iter().map(str::to_owned).collect(),
                )
            })
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn source_repository_links_registrations_with_unrelated_names() {
        let unsloth = PathBuf::from(
            "/data/hf/models--unsloth--Qwen3.8-27B-GGUF/snapshots/abc/Qwen3.8-27B-UD-Q4_K_XL.gguf",
        );
        let other =
            PathBuf::from("/data/hf/models--bartowski--Qwen3.8-27B-GGUF/snapshots/d/x.gguf");
        let plain = PathBuf::from("/data/models/other.gguf");
        assert_eq!(
            repository_name(&unsloth).as_deref(),
            Some("Qwen3.8-27B-GGUF")
        );
        assert_eq!(repository_name(&plain), None);
        let found = groups([
            ("my-daily-driver", unsloth.as_path()),
            ("Qwen3.8-27B-GGUF-UD-Q8_K_XL", other.as_path()),
            ("unrelated", plain.as_path()),
        ]);
        assert_eq!(
            found,
            vec![Group {
                id: "qwen3.8-27b".into(),
                label: "Qwen3.8-27B".into(),
                members: vec![
                    "Qwen3.8-27B-GGUF-UD-Q8_K_XL".into(),
                    "my-daily-driver".into()
                ],
            }]
        );
    }
}
