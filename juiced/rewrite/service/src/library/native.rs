//! Native `lemonade` checkpoint vocabulary (`owner/repo:VARIANT`, registry names) resolved onto
//! the library's verified Hugging Face pulls.

use super::*;

/// A pull the library can start: one repository and the files of exactly one variant.
#[derive(Clone, Debug)]
pub struct Selection {
    pub repo: String,
    pub files: Vec<String>,
    pub bytes: Option<u64>,
    pub label: Option<String>,
}

/// Built-in registry entry for a native model name.
#[derive(Clone, Debug)]
pub struct Builtin {
    pub repo: String,
    pub file: Option<String>,
    pub quant: Option<String>,
}

/// Splits `owner/repo[:VARIANT]`; a bare `owner/repo` selects the recommended variant.
pub fn parse_checkpoint(value: &str) -> Option<(String, Option<String>)> {
    let value = value.trim();
    let (repo, variant) = match value.split_once(':') {
        Some((repo, variant)) => (repo, Some(variant.trim()).filter(|text| !text.is_empty())),
        None => (value, None),
    };
    valid_repo(repo).then(|| (repo.to_owned(), variant.map(str::to_owned)))
}

pub fn builtin(name: &str) -> Option<Builtin> {
    catalog::catalog()
        .models
        .iter()
        .find(|model| model.native_id.eq_ignore_ascii_case(name) || model.id == name)
        .map(|model| Builtin {
            repo: model.repo.clone(),
            file: model.recommended_file.clone(),
            quant: model.recommended_quant.clone(),
        })
}

fn rank(label: Option<&str>) -> usize {
    const PREFERRED: [&str; 4] = ["Q4_K_M", "UD-Q4_K_XL", "Q8_0", "Q4_0"];
    label
        .and_then(|label| {
            PREFERRED
                .iter()
                .position(|known| known.eq_ignore_ascii_case(label))
        })
        .unwrap_or(PREFERRED.len())
}

fn matches(variant: &quant::Variant, wanted: &str) -> bool {
    variant
        .label
        .as_deref()
        .is_some_and(|label| label.eq_ignore_ascii_case(wanted))
        || variant.files.iter().any(|file| {
            file.path.eq_ignore_ascii_case(wanted)
                || quant::basename(&file.path).eq_ignore_ascii_case(wanted)
        })
}

fn mmproj(path: &str) -> bool {
    quant::basename(path)
        .to_ascii_lowercase()
        .starts_with("mmproj")
}

impl Library {
    async fn repo_variants(
        &self,
        repo: &str,
    ) -> Result<(Arc<hub::RepoInfo>, Vec<quant::Variant>), ApiError> {
        if self.policy == SourcePolicy::Offline {
            return Err(ArtifactError::Offline.into());
        }
        if let Some(error) = self.hub.blocked() {
            return Err(hub_api_error(error));
        }
        let (info, _) = self
            .hub
            .info(repo, None, false)
            .await
            .map_err(hub_api_error)?;
        let mut variants = quant::group(&info.files);
        variants.retain(|variant| !mmproj(&variant.primary));
        let bundles: Vec<_> = catalog::catalog()
            .models
            .iter()
            .filter(|model| model.repo == repo)
            .filter_map(|model| model.bundle(&info.files))
            .collect();
        variants.splice(0..0, bundles);
        Ok((info, variants))
    }

    /// Picks the variant a native checkpoint names, or the recommended one when it names none.
    pub async fn select_checkpoint(
        &self,
        repo: &str,
        variant: Option<&str>,
        recommended: Option<&str>,
    ) -> Result<Selection, ApiError> {
        let (_, variants) = self.repo_variants(repo).await?;
        let chosen = match variant.or(recommended) {
            Some(wanted) => variants.iter().find(|variant| matches(variant, wanted)),
            None => variants
                .iter()
                .filter(|variant| variant.complete)
                .min_by_key(|variant| rank(variant.label.as_deref())),
        }
        .ok_or_else(|| {
            library_error(
                StatusCode::NOT_FOUND,
                "library_file_not_found",
                "The checkpoint names no model file or quantization in this repository.",
            )
        })?;
        Ok(Selection {
            repo: repo.to_owned(),
            files: chosen.files.iter().map(|file| file.path.clone()).collect(),
            bytes: chosen.bytes(),
            label: chosen.label.clone(),
        })
    }

    /// The native `GET /pull/variants` body for one repository.
    pub async fn native_variants(&self, repo: &str) -> Result<Value, ApiError> {
        let (info, variants) = self.repo_variants(repo).await?;
        let mut ranked: Vec<&quant::Variant> = variants
            .iter()
            .filter(|variant| variant.complete && variant.roles.is_empty())
            .collect();
        ranked.sort_by(|left, right| {
            rank(left.label.as_deref())
                .cmp(&rank(right.label.as_deref()))
                .then_with(|| left.label.cmp(&right.label))
                .then_with(|| left.primary.cmp(&right.primary))
        });
        let mmproj_files: Vec<&str> = info
            .files
            .iter()
            .filter(|file| mmproj(&file.path) && quant::is_listed_file(&file.path))
            .map(|file| quant::basename(&file.path))
            .collect();
        let lower = repo.to_ascii_lowercase();
        let mut labels = Vec::new();
        if !mmproj_files.is_empty() {
            labels.push("vision");
        }
        if lower.contains("embed") {
            labels.push("embeddings");
        }
        if lower.contains("rerank") {
            labels.push("reranking");
        }
        let variants: Vec<Value> = ranked
            .into_iter()
            .take(5)
            .map(|variant| {
                json!({
                    "name": variant.label.clone().unwrap_or_else(|| quant::basename(&variant.primary).trim_end_matches(".gguf").to_owned()),
                    "primary_file": variant.primary,
                    "files": variant.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(),
                    "sharded": variant.parts > 1,
                    "size_bytes": variant.bytes(),
                })
            })
            .collect();
        Ok(json!({
            "checkpoint": repo,
            "recipe": "llamacpp",
            "suggested_name": repo.split_once('/').map_or(repo, |(_, name)| name),
            "suggested_labels": labels,
            "mmproj_files": mmproj_files,
            "variants": variants,
        }))
    }
}
