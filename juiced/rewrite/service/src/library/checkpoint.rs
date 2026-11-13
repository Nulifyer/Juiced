//! Original checkpoint folders on the hub: Transformers safetensors for vLLM, exported ONNX
//! classifiers for ONNX Runtime and split diffusion safetensors for TheNoise. Each becomes one
//! variant whose files publish as an artifact set the engines read as a directory.

use super::quant::{self, FileMeta, Variant};
use crate::{artifacts::ArtifactRole, engines::catalog::Engine};
use serde::{Deserialize, Serialize};

/// Files outside LFS carry no SHA-256 on the hub; the pull fetches and checks them first.
pub const SMALL_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_MEMBERS: usize = 64;
const MEMBER_NAME_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Transformers,
    OnnxClassifier,
    #[serde(rename = "thenoise-diffusion")]
    TheNoiseDiffusion,
}

impl Kind {
    pub fn engine(self) -> Engine {
        match self {
            Self::Transformers => Engine::Vllm,
            Self::OnnxClassifier => Engine::OnnxRuntime,
            Self::TheNoiseDiffusion => Engine::TheNoise,
        }
    }
    pub fn for_engine(engine: Engine) -> Option<Self> {
        [
            Self::Transformers,
            Self::OnnxClassifier,
            Self::TheNoiseDiffusion,
        ]
        .into_iter()
        .find(|kind| kind.engine() == engine)
    }
    /// The hub tag that narrows a search to repositories with this kind of weights.
    pub fn hub_filter(self) -> &'static str {
        match self {
            Self::Transformers | Self::TheNoiseDiffusion => "safetensors",
            Self::OnnxClassifier => "onnx",
        }
    }
}

const TOKENIZERS: &[&str] = &[
    "tokenizer.json",
    "tokenizer_config.json",
    "tokenizer.model",
    "special_tokens_map.json",
    "added_tokens.json",
];
const VOCABULARIES: &[&str] = &["vocab.json", "vocab.txt", "merges.txt"];
const CONFIGURATIONS: &[&str] = &[
    "generation_config.json",
    "chat_template.jinja",
    "chat_template.json",
];
const PROCESSORS: &[&str] = &[
    "preprocessor_config.json",
    "processor_config.json",
    "video_preprocessor_config.json",
];
const INDEX: &str = "model.safetensors.index.json";

fn lower_basename(path: &str) -> String {
    quant::basename(path).to_ascii_lowercase()
}

/// Repository files a checkpoint may need beyond what the GGUF listing keeps.
pub fn is_listed(path: &str) -> bool {
    let name = lower_basename(path);
    name == "config.json"
        || name == INDEX
        || name.ends_with(".onnx_data")
        || name.ends_with(".onnx.data")
        || [TOKENIZERS, VOCABULARIES, CONFIGURATIONS, PROCESSORS]
            .iter()
            .any(|names| names.contains(&name.as_str()))
}

fn role_for(name: &str) -> Option<ArtifactRole> {
    let name = name.to_ascii_lowercase();
    let role = if name == INDEX {
        ArtifactRole::WeightIndex
    } else if name.ends_with(".safetensors")
        || name.ends_with(".onnx_data")
        || name.ends_with(".onnx.data")
    {
        ArtifactRole::ExternalTensor
    } else if TOKENIZERS.contains(&name.as_str()) {
        ArtifactRole::Tokenizer
    } else if VOCABULARIES.contains(&name.as_str()) {
        ArtifactRole::Vocabulary
    } else if name == "config.json" || CONFIGURATIONS.contains(&name.as_str()) {
        ArtifactRole::Configuration
    } else if PROCESSORS.contains(&name.as_str()) {
        ArtifactRole::Processor
    } else {
        return None;
    };
    Some(role)
}

fn member_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MEMBER_NAME_BYTES
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// Builds a variant from `(file, member name, role)` rows; the first row is the primary.
fn variant(
    label: String,
    rows: Vec<(&FileMeta, String, ArtifactRole)>,
    parts: u32,
    complete: bool,
    kind: Kind,
) -> Option<Variant> {
    if rows.len() < 2 || rows.len() > MAX_MEMBERS {
        return None;
    }
    let mut names = std::collections::BTreeSet::new();
    if rows
        .iter()
        .any(|(_, name, _)| !member_name(name) || !names.insert(name.clone()))
    {
        return None;
    }
    Some(Variant {
        label: Some(label),
        primary: rows[0].0.path.clone(),
        files: rows.iter().map(|(file, _, _)| (*file).clone()).collect(),
        parts,
        complete,
        roles: rows.iter().map(|(_, _, role)| *role).collect(),
        names: rows.into_iter().map(|(_, name, _)| name).collect(),
        checkpoint: Some(kind),
    })
}

fn shard(name: &str) -> Option<(u32, u32)> {
    let stem = name.strip_suffix(".safetensors")?;
    let (head, total) = stem.rsplit_once("-of-")?;
    let (_, index) = head.rsplit_once('-')?;
    let (index, total) = (index.parse().ok()?, total.parse().ok()?);
    (index >= 1 && index <= total).then_some((index, total))
}

/// A Transformers checkpoint: `config.json` beside root safetensors weights.
fn transformers(files: &[FileMeta]) -> Option<Variant> {
    let root: Vec<&FileMeta> = files
        .iter()
        .filter(|file| !file.path.contains('/'))
        .collect();
    let config = root.iter().find(|file| file.path == "config.json")?;
    let mut weights: Vec<&FileMeta> = root
        .iter()
        .filter(|file| file.path.to_ascii_lowercase().ends_with(".safetensors"))
        .copied()
        .collect();
    if weights
        .iter()
        .any(|file| !file.path.starts_with("consolidated"))
    {
        weights.retain(|file| !file.path.starts_with("consolidated"));
    }
    if weights.is_empty() {
        return None;
    }
    weights.sort_by(|left, right| left.path.cmp(&right.path));
    let shards: Vec<(u32, u32)> = weights
        .iter()
        .filter_map(|file| shard(&file.path))
        .collect();
    let complete = match shards.first() {
        Some((_, total)) if shards.len() == weights.len() => {
            shards.iter().all(|(_, known)| known == total)
                && shards.len() == *total as usize
                && root.iter().any(|file| file.path == INDEX)
        }
        _ => true,
    };
    let mut rows = vec![((*config), "config.json".to_owned(), ArtifactRole::Model)];
    for file in &weights {
        rows.push((*file, file.path.clone(), ArtifactRole::ExternalTensor));
    }
    for file in &root {
        if file.path == "config.json" || weights.iter().any(|known| known.path == file.path) {
            continue;
        }
        if let Some(role) =
            role_for(&file.path).filter(|role| *role != ArtifactRole::ExternalTensor)
        {
            rows.push((*file, file.path.clone(), role));
        }
    }
    variant(
        "Checkpoint".into(),
        rows,
        weights.len() as u32,
        complete,
        Kind::Transformers,
    )
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn onnx_label(name: &str) -> String {
    match name
        .strip_suffix(".onnx")
        .and_then(|stem| stem.strip_prefix("model_"))
    {
        Some(suffix) if !suffix.is_empty() => format!("ONNX {}", suffix.to_ascii_uppercase()),
        _ => "ONNX".into(),
    }
}

/// Exported classifiers: `model*.onnx` at the root or under `onnx/`, with the tokenizer and
/// config from the same folder or the root. ort-server reads the graph as `model.onnx`.
fn onnx_classifiers(files: &[FileMeta]) -> Vec<Variant> {
    let find = |folder: &str, name: &str| {
        let nested = format!("{folder}/{name}");
        files
            .iter()
            .find(|file| !folder.is_empty() && file.path == nested)
            .or_else(|| files.iter().find(|file| file.path == name))
    };
    let mut variants = Vec::new();
    for graph in files {
        let name = quant::basename(&graph.path);
        let folder = directory(&graph.path);
        if !matches!(folder, "" | "onnx") || !name.starts_with("model") || !name.ends_with(".onnx")
        {
            continue;
        }
        let (Some(tokenizer), Some(config)) =
            (find(folder, "tokenizer.json"), find(folder, "config.json"))
        else {
            continue;
        };
        let mut rows = vec![
            (graph, "model.onnx".to_owned(), ArtifactRole::Model),
            (tokenizer, "tokenizer.json".into(), ArtifactRole::Tokenizer),
            (config, "config.json".into(), ArtifactRole::Configuration),
        ];
        for data in [format!("{name}_data"), format!("{name}.data")] {
            if let Some(file) = find(folder, &data).filter(|file| directory(&file.path) == folder) {
                rows.push((file, data, ArtifactRole::ExternalTensor));
            }
        }
        for extra in [
            "tokenizer_config.json",
            "special_tokens_map.json",
            "vocab.txt",
            "vocab.json",
            "merges.txt",
        ] {
            if let Some(file) = find(folder, extra) {
                rows.push((
                    file,
                    extra.to_owned(),
                    role_for(extra).unwrap_or(ArtifactRole::Tokenizer),
                ));
            }
        }
        if let Some(found) = variant(onnx_label(name), rows, 1, true, Kind::OnnxClassifier) {
            variants.push(found);
        }
    }
    variants
}

fn component(path: &str) -> Option<ArtifactRole> {
    if !path.to_ascii_lowercase().ends_with(".safetensors") {
        return None;
    }
    let folders: Vec<&str> = path.split('/').rev().skip(1).collect();
    let within = |name: &str| folders.contains(&name);
    if within("diffusion_models") {
        Some(ArtifactRole::Model)
    } else if within("vae") {
        Some(ArtifactRole::Vae)
    } else if within("text_encoders") {
        Some(ArtifactRole::Encoder)
    } else {
        None
    }
}

const PRECISIONS: &[&str] = &["bf16", "fp16", "fp8", "int8", "fp32", "nvfp4", "mxfp4"];

/// Picks the one candidate, or the one sharing the denoiser's precision tag.
fn paired<'a>(candidates: &[&'a FileMeta], denoiser: &str) -> Option<&'a FileMeta> {
    if let [only] = candidates {
        return Some(only);
    }
    let tag = PRECISIONS
        .iter()
        .find(|tag| denoiser.to_ascii_lowercase().contains(*tag))?;
    let matching: Vec<_> = candidates
        .iter()
        .filter(|file| file.path.to_ascii_lowercase().contains(tag))
        .collect();
    match matching.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

/// ComfyUI-style split repositories (`diffusion_models/`, `vae/`, `text_encoders/`): one
/// variant per diffusion model, paired with its VAE and text encoder.
fn thenoise(files: &[FileMeta]) -> Vec<Variant> {
    let of = |role| -> Vec<&FileMeta> {
        files
            .iter()
            .filter(|file| component(&file.path) == Some(role))
            .collect()
    };
    let (denoisers, vaes, encoders) = (
        of(ArtifactRole::Model),
        of(ArtifactRole::Vae),
        of(ArtifactRole::Encoder),
    );
    denoisers
        .into_iter()
        .filter_map(|denoiser| {
            let vae = paired(&vaes, &denoiser.path)?;
            let encoder = paired(&encoders, &denoiser.path)?;
            let label = quant::basename(&denoiser.path)
                .trim_end_matches(".safetensors")
                .chars()
                .take(48)
                .collect();
            variant(
                label,
                vec![
                    (
                        denoiser,
                        quant::basename(&denoiser.path).to_owned(),
                        ArtifactRole::Model,
                    ),
                    (
                        vae,
                        quant::basename(&vae.path).to_owned(),
                        ArtifactRole::Vae,
                    ),
                    (
                        encoder,
                        quant::basename(&encoder.path).to_owned(),
                        ArtifactRole::Encoder,
                    ),
                ],
                1,
                true,
                Kind::TheNoiseDiffusion,
            )
        })
        .collect()
}

/// Every checkpoint variant of a repository revision. ONNX graphs count as classifiers only
/// when the hub tags the repository for text classification, since Kokoro and other ONNX
/// exports share the layout.
pub fn variants(files: &[FileMeta], pipeline: Option<&str>) -> Vec<Variant> {
    let mut found: Vec<Variant> = transformers(files).into_iter().collect();
    if pipeline == Some("text-classification") {
        found.extend(onnx_classifiers(files));
    }
    found.extend(thenoise(files));
    found
}

/// The member names a pull publishes for this variant's files, in file order.
pub fn member_names(variant: &Variant) -> Option<&[String]> {
    (variant.names.len() == variant.files.len()).then_some(variant.names.as_slice())
}

/// The git blob id of `bytes` (`sha1("blob <len>\0" + bytes)`), which the hub reports for
/// files stored outside LFS.
pub fn git_blob(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()).as_bytes());
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, bytes: u64, lfs: bool) -> FileMeta {
        FileMeta {
            path: path.into(),
            bytes: Some(bytes),
            sha256: lfs.then(|| "a".repeat(64)),
            blob: (!lfs).then(|| "b".repeat(40)),
        }
    }

    #[test]
    fn transformers_repositories_become_one_checkpoint_with_their_tokenizer_files() {
        let files = [
            file("config.json", 726, false),
            file("generation_config.json", 239, false),
            file("merges.txt", 1_671_853, false),
            file("model.safetensors", 1_503_300_328, true),
            file("tokenizer.json", 11_422_654, true),
            file("tokenizer_config.json", 9732, false),
            file("vocab.json", 2_776_833, false),
            file("onnx/model.onnx", 10, true),
        ];
        let found = variants(&files, Some("text-generation"));
        assert_eq!(found.len(), 1);
        let checkpoint = &found[0];
        assert_eq!(checkpoint.checkpoint, Some(Kind::Transformers));
        assert_eq!(checkpoint.primary, "config.json");
        assert_eq!(checkpoint.names[0], "config.json");
        assert_eq!(checkpoint.roles[0], ArtifactRole::Model);
        assert_eq!(checkpoint.files.len(), 7);
        assert!(checkpoint.verifiable());
        assert_eq!(checkpoint.bytes(), Some(1_519_182_365));
        assert!(
            !checkpoint
                .files
                .iter()
                .any(|file| file.path.contains("onnx"))
        );
    }

    #[test]
    fn sharded_checkpoints_need_every_shard_and_their_index() {
        let mut files = vec![
            file("config.json", 10, false),
            file("model-00001-of-00002.safetensors", 100, true),
            file("model-00002-of-00002.safetensors", 100, true),
        ];
        assert!(!variants(&files, None)[0].complete);
        files.push(file(INDEX, 5, false));
        let complete = &variants(&files, None)[0];
        assert!(complete.complete);
        assert_eq!(complete.parts, 2);
        assert!(complete.roles.contains(&ArtifactRole::WeightIndex));
        files.retain(|file| !file.path.starts_with("model-00002"));
        assert!(!variants(&files, None)[0].complete);
    }

    #[test]
    fn onnx_classifiers_need_the_classification_tag_and_publish_model_onnx() {
        let files = [
            file("config.json", 791, false),
            file("model.onnx", 267_938_324, true),
            file("tokenizer.json", 711_661, false),
            file("onnx/model_quantized.onnx", 1000, true),
            file("vocab.txt", 231_508, false),
            file("special_tokens_map.json", 732, false),
        ];
        assert!(variants(&files, Some("text-to-speech")).is_empty());
        let found = variants(&files, Some("text-classification"));
        let labels: Vec<_> = found
            .iter()
            .map(|found| found.label.clone().unwrap())
            .collect();
        assert_eq!(labels, ["ONNX", "ONNX QUANTIZED"]);
        assert!(found.iter().all(|found| found.names[0] == "model.onnx"));
        assert_eq!(found[1].primary, "onnx/model_quantized.onnx");
        assert_eq!(found[0].files.len(), 5);
    }

    #[test]
    fn split_diffusion_repositories_pair_each_model_with_its_vae_and_encoder() {
        let files = [
            file(
                "split_files/diffusion_models/z_image_turbo_bf16.safetensors",
                9,
                true,
            ),
            file(
                "split_files/diffusion_models/z_image_turbo_fp8.safetensors",
                8,
                true,
            ),
            file("split_files/vae/ae.safetensors", 3, true),
            file("split_files/text_encoders/qwen_3_4b.safetensors", 4, true),
        ];
        let found = variants(&files, None);
        assert_eq!(found.len(), 2);
        assert!(
            found
                .iter()
                .all(|found| found.checkpoint == Some(Kind::TheNoiseDiffusion))
        );
        assert_eq!(found[0].label.as_deref(), Some("z_image_turbo_bf16"));
        assert_eq!(
            found[0].roles,
            [
                ArtifactRole::Model,
                ArtifactRole::Vae,
                ArtifactRole::Encoder
            ]
        );
        let ambiguous = [
            file("diffusion_models/a.safetensors", 9, true),
            file("vae/one.safetensors", 3, true),
            file("vae/two.safetensors", 3, true),
            file("text_encoders/t.safetensors", 4, true),
        ];
        assert!(variants(&ambiguous, None).is_empty());
    }

    #[test]
    fn git_blob_ids_match_git() {
        assert_eq!(
            git_blob(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }
}
