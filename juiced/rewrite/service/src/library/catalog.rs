use super::{Task, quant, valid_repo};
use serde::Deserialize;
use std::{collections::BTreeSet, sync::OnceLock};

const SEED: &str = include_str!("catalog.json");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub repo: String,
    pub family: Option<String>,
    pub task: Task,
    pub recommended_quant: Option<String>,
    pub recommended_file: Option<String>,
    pub variant_stem: Option<String>,
    pub license: Option<String>,
    pub native_id: String,
    #[serde(default)]
    pub companions: Vec<CatalogCompanion>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCompanion {
    pub file: String,
    pub role: crate::artifacts::ArtifactRole,
}

const MAX_COMPANIONS: usize = 8;

fn bundle_primary(file: &str) -> bool {
    let name = file.to_ascii_lowercase();
    name.ends_with(".onnx") || name.ends_with(".safetensors") || name.ends_with(".gguf")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema_version: u32,
    pub source: String,
    pub models: Vec<CatalogModel>,
}

impl CatalogModel {
    pub fn matches(&self, terms: &[String], task: Option<Task>) -> bool {
        if task.is_some_and(|task| task != self.task) {
            return false;
        }
        let haystack = format!(
            "{} {} {} {}",
            self.id,
            self.name,
            self.repo,
            self.family.as_deref().unwrap_or("")
        )
        .to_ascii_lowercase();
        terms.iter().all(|term| haystack.contains(term.as_str()))
    }
    pub fn includes(&self, path: &str) -> bool {
        self.variant_stem
            .as_deref()
            .is_none_or(|stem| quant::stem(path) == stem.to_ascii_lowercase())
    }
    pub fn bundle(&self, files: &[quant::FileMeta]) -> Option<quant::Variant> {
        if self.companions.is_empty() {
            return None;
        }
        let companions: Vec<_> = self
            .companions
            .iter()
            .map(|companion| (companion.file.clone(), companion.role))
            .collect();
        quant::bundle(files, self.recommended_file.as_deref()?, &companions)
    }
    pub fn recommends(&self, label: Option<&str>, primary: &str) -> bool {
        match (&self.recommended_file, &self.recommended_quant) {
            (Some(file), _) => quant::basename(primary).eq_ignore_ascii_case(file),
            (None, Some(recommended)) => label == Some(recommended.as_str()),
            (None, None) => false,
        }
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte))
}

fn text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

impl Catalog {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.source != "src/cpp/resources/server_models.json"
            || self.models.is_empty()
            || self.models.len() > 256
        {
            return Err("Unsupported catalog schema".into());
        }
        let mut ids = BTreeSet::new();
        for model in &self.models {
            let fail = |reason: &str| Err(format!("Catalog entry {}: {reason}", model.id));
            if !identifier(&model.id) || !ids.insert(model.id.as_str()) {
                return fail("identifier must be unique lowercase ASCII");
            }
            if !text(&model.name, 96) || !text(&model.native_id, 128) {
                return fail("name and native identifier are required");
            }
            if !valid_repo(&model.repo) {
                return fail("repository must be a public owner/name identifier");
            }
            if model
                .family
                .as_deref()
                .is_some_and(|family| !identifier(family))
            {
                return fail("family must be a lowercase identifier");
            }
            if model
                .license
                .as_deref()
                .is_some_and(|license| !identifier(license))
            {
                return fail("license must be a lowercase SPDX-like identifier");
            }
            match (&model.recommended_quant, &model.recommended_file) {
                (Some(label), None)
                    if quant::quant_label(label).as_deref() == Some(label)
                        && model.companions.is_empty() => {}
                (None, Some(file))
                    if (quant::is_model_file(file)
                        || (!model.companions.is_empty() && bundle_primary(file)))
                        && !file.contains('/')
                        && text(file, 128) => {}
                _ => return fail("exactly one parseable recommended quant or file is required"),
            }
            if model.companions.len() > MAX_COMPANIONS
                || model.companions.iter().any(|companion| {
                    !super::valid_file(&companion.file)
                        || Some(&companion.file) == model.recommended_file.as_ref()
                        || !matches!(
                            companion.role,
                            crate::artifacts::ArtifactRole::Voice
                                | crate::artifacts::ArtifactRole::Vae
                                | crate::artifacts::ArtifactRole::Encoder
                                | crate::artifacts::ArtifactRole::Decoder
                        )
                })
            {
                return fail(
                    "companions need distinct repository files with voice, vae, encoder or decoder roles",
                );
            }
            if let Some(stem) = &model.variant_stem {
                let file = model.recommended_file.as_deref().unwrap_or("");
                if !text(stem, 128) || quant::stem(file) != stem.to_ascii_lowercase() {
                    return fail("variant stem must select the recommended file");
                }
            }
        }
        Ok(())
    }
}

pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let catalog: Catalog =
            serde_json::from_str(SEED).expect("compiled library catalog is valid JSON");
        catalog
            .validate()
            .expect("compiled library catalog passes its schema check");
        catalog
    })
}
