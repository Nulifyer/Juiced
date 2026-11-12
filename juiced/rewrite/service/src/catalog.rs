use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};

const SEED: &str = include_str!("../../catalog-design/inventory.json");

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDefinition {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub priority: String,
    pub candidate_backends: Vec<String>,
    pub tasks: Vec<String>,
    pub sources: Vec<String>,
    pub notes: String,
    pub upstream_lifecycle: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComputePathDefinition {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSupportRow {
    pub backend: String,
    pub tier: String,
    pub format: String,
    pub os: Vec<String>,
    pub device_declaration: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileOverride {
    pub legacy_backend: String,
    pub catalog_entry: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeMigration {
    pub legacy_recipe: String,
    pub display_name: String,
    pub catalog_entry: String,
    pub native_selectable_backend: bool,
    pub native_uses_ctx_size: bool,
    pub native_backend_labels: Vec<String>,
    pub migration_rule: String,
    pub native_support_rows: Vec<NativeSupportRow>,
    #[serde(default)]
    pub profile_overrides: Vec<ProfileOverride>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeReference {
    pub source_head: String,
    pub source_tree_state: String,
    pub source_files: Vec<SourceFile>,
    pub recipe_count: usize,
    pub support_row_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFamilyExample {
    pub id: String,
    pub runtime_candidates: Vec<String>,
    pub notes: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seed {
    schema_version: u32,
    purpose: String,
    reviewed_at: String,
    execution_enabled: bool,
    priority_definitions: std::collections::BTreeMap<String, String>,
    candidate_backend_semantics: String,
    backends: Vec<ComputePathDefinition>,
    tasks: Vec<String>,
    native_reference: NativeReference,
    native_recipe_migrations: Vec<NativeMigration>,
    entries: Vec<RuntimeDefinition>,
    model_family_examples: Vec<ModelFamilyExample>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    Planned,
    Partial,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterImplementation {
    pub status: AdapterStatus,
    pub scope: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DefinitionProjection<'a> {
    #[serde(flatten)]
    pub definition: &'a RuntimeDefinition,
    pub adapter: AdapterImplementation,
}

#[derive(Debug, Serialize)]
pub struct CatalogProjection<'a> {
    pub schema_version: u32,
    pub purpose: &'a str,
    pub reviewed_at: &'a str,
    pub execution_enabled: bool,
    pub candidate_backend_semantics: &'a str,
    pub priority_definitions: &'a std::collections::BTreeMap<String, String>,
    pub compute_paths: &'a [ComputePathDefinition],
    pub discovery_tasks: &'a [String],
    pub definitions: Vec<DefinitionProjection<'a>>,
    pub native_reference: &'a NativeReference,
    pub native_recipe_migrations: &'a [NativeMigration],
    pub model_family_examples: &'a [ModelFamilyExample],
    pub qualification_receipts: [(); 0],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionBlockerCode {
    BundleIdentityRequired,
    DeviceIdentityRequired,
    InstallationProvenanceUnresolved,
    RecipeScopedNpuStackRequired,
    HybridPlacementRequired,
    AdapterNotImplemented,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolutionBlocker {
    pub code: ResolutionBlockerCode,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct LegacyResolution {
    pub legacy_recipe: String,
    pub legacy_backend: String,
    pub implementation_id: String,
    pub compute_path_id: Option<String>,
    pub evidence_scope: &'static str,
    pub migration_rule: String,
    pub override_reason: Option<String>,
    pub native_support_rows: Vec<NativeSupportRow>,
    pub adapter: AdapterImplementation,
    pub execution_enabled: bool,
    pub blockers: Vec<ResolutionBlocker>,
    pub qualification_receipts: [(); 0],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogError {
    pub code: &'static str,
    pub message: String,
}

impl CatalogError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_catalog_seed", message)
    }
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CatalogError {}

#[derive(Debug)]
pub struct Catalog {
    seed: Seed,
}

impl Catalog {
    pub fn from_seed() -> Result<Self, CatalogError> {
        Self::from_json(SEED)
    }

    pub fn from_json(json: &str) -> Result<Self, CatalogError> {
        let seed = serde_json::from_str::<Seed>(json)
            .map_err(|_| CatalogError::invalid("Catalog JSON does not match schema version 1."))?;
        validate(&seed)?;
        Ok(Self { seed })
    }

    pub fn definitions(&self) -> &[RuntimeDefinition] {
        &self.seed.entries
    }

    pub fn definition(&self, id: &str) -> Option<&RuntimeDefinition> {
        self.seed.entries.iter().find(|entry| entry.id == id)
    }

    pub fn native_recipe_migrations(&self) -> &[NativeMigration] {
        &self.seed.native_recipe_migrations
    }

    pub fn projection(&self) -> CatalogProjection<'_> {
        CatalogProjection {
            schema_version: self.seed.schema_version,
            purpose: &self.seed.purpose,
            reviewed_at: &self.seed.reviewed_at,
            execution_enabled: false,
            candidate_backend_semantics: &self.seed.candidate_backend_semantics,
            priority_definitions: &self.seed.priority_definitions,
            compute_paths: &self.seed.backends,
            discovery_tasks: &self.seed.tasks,
            definitions: self
                .definitions()
                .iter()
                .map(|definition| DefinitionProjection {
                    definition,
                    adapter: adapter(&definition.id),
                })
                .collect(),
            native_reference: &self.seed.native_reference,
            native_recipe_migrations: &self.seed.native_recipe_migrations,
            model_family_examples: &self.seed.model_family_examples,
            qualification_receipts: [],
        }
    }

    pub fn resolve_legacy(
        &self,
        recipe: &str,
        backend: &str,
    ) -> Result<LegacyResolution, CatalogError> {
        let migration = self
            .seed
            .native_recipe_migrations
            .iter()
            .find(|migration| migration.legacy_recipe == recipe)
            .ok_or_else(|| {
                CatalogError::new(
                    "unknown_legacy_recipe",
                    "Unknown exact native recipe label.",
                )
            })?;
        if !migration
            .native_backend_labels
            .iter()
            .any(|id| id == backend)
        {
            return Err(CatalogError::new(
                "unknown_legacy_backend",
                "Backend label is not declared for this exact native recipe.",
            ));
        }
        let profile_override = migration
            .profile_overrides
            .iter()
            .find(|profile| profile.legacy_backend == backend);
        let implementation_id = profile_override
            .map(|profile| &profile.catalog_entry)
            .unwrap_or(&migration.catalog_entry)
            .clone();
        let adapter = adapter(&implementation_id);
        let mut blockers = vec![
            ResolutionBlocker {
                code: ResolutionBlockerCode::BundleIdentityRequired,
                message: "Select and verify an immutable runtime bundle before execution.".into(),
            },
            ResolutionBlocker {
                code: ResolutionBlockerCode::DeviceIdentityRequired,
                message: "Select actual devices and validate the native row restrictions.".into(),
            },
        ];
        let compute_path_id = match backend {
            "system" => {
                blockers.push(ResolutionBlocker {
                    code: ResolutionBlockerCode::InstallationProvenanceUnresolved,
                    message: "system names installation provenance. The actual build and compute path remain unknown.".into(),
                });
                None
            }
            "npu" => {
                blockers.push(ResolutionBlocker {
                    code: ResolutionBlockerCode::RecipeScopedNpuStackRequired,
                    message: "npu names hardware. Resolve this recipe's exact vendor stack and stage placement from its selected bundle.".into(),
                });
                None
            }
            "hybrid-rocm" => {
                blockers.push(ResolutionBlocker {
                    code: ResolutionBlockerCode::HybridPlacementRequired,
                    message: "The pinned hybrid distribution requires explicit NPU/GPU placement and its exact bundle.".into(),
                });
                None
            }
            "rocm" => Some("hip".to_owned()),
            id => Some(id.to_owned()),
        };
        if adapter.status == AdapterStatus::Planned {
            blockers.push(ResolutionBlocker {
                code: ResolutionBlockerCode::AdapterNotImplemented,
                message: "The Rust runtime adapter for this implementation is planned.".into(),
            });
        }
        Ok(LegacyResolution {
            legacy_recipe: recipe.to_owned(),
            legacy_backend: backend.to_owned(),
            implementation_id,
            compute_path_id,
            evidence_scope: "Native metadata declaration and compute vocabulary only. No installed bundle, actual device, operation, or Rust execution qualification is established.",
            migration_rule: migration.migration_rule.clone(),
            override_reason: profile_override.map(|profile| profile.reason.clone()),
            native_support_rows: migration
                .native_support_rows
                .iter()
                .filter(|row| row.backend == backend)
                .cloned()
                .collect(),
            adapter,
            execution_enabled: false,
            blockers,
            qualification_receipts: [],
        })
    }
}

fn adapter(implementation_id: &str) -> AdapterImplementation {
    if implementation_id == "llama-cpp" || implementation_id == "whisper-cpp" {
        AdapterImplementation {
            status: AdapterStatus::Partial,
            scope: if implementation_id == "llama-cpp" {
                "Selected llama.cpp managed subprocess HTTP profile only. Catalog candidates and native backend rows are not adapter qualification."
            } else {
                "Selected whisper.cpp single-slot multipart transcription subprocess only. Runtime bundles, devices, native backend rows and model quality need separate qualification."
            },
        }
    } else {
        AdapterImplementation {
            status: AdapterStatus::Planned,
            scope: "Definition only. No Rust runtime adapter or qualification receipt.",
        }
    }
}

fn identifier(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn unique<'a>(
    values: impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<BTreeSet<&'a str>, CatalogError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.is_empty() || !seen.insert(value) {
            return Err(CatalogError::invalid(format!(
                "Invalid or duplicate {label}."
            )));
        }
    }
    Ok(seen)
}

fn require(condition: bool, message: &str) -> Result<(), CatalogError> {
    if condition {
        Ok(())
    } else {
        Err(CatalogError::invalid(message))
    }
}

fn validate(seed: &Seed) -> Result<(), CatalogError> {
    require(seed.schema_version == 1, "Unknown catalog schema version.")?;
    require(
        !seed.execution_enabled,
        "A catalog seed cannot enable execution.",
    )?;
    require(
        !seed.purpose.is_empty()
            && !seed.candidate_backend_semantics.is_empty()
            && seed.reviewed_at.len() == 10,
        "Incomplete catalog metadata.",
    )?;
    let entries = unique(
        seed.entries.iter().map(|entry| entry.id.as_str()),
        "runtime ID",
    )?;
    let backends = unique(
        seed.backends.iter().map(|backend| backend.id.as_str()),
        "compute path ID",
    )?;
    let tasks = unique(seed.tasks.iter().map(String::as_str), "discovery task")?;
    let families = unique(
        seed.model_family_examples
            .iter()
            .map(|family| family.id.as_str()),
        "model family ID",
    )?;
    require(
        entries
            .iter()
            .chain(&backends)
            .chain(&families)
            .all(|id| identifier(id)),
        "Invalid catalog identity.",
    )?;
    require(
        entries.is_disjoint(&families),
        "Runtime and model family IDs collide.",
    )?;
    let priorities = [
        "parity",
        "first-expansion",
        "conditional",
        "attach",
        "reference-only",
        "legacy",
    ];
    require(
        seed.priority_definitions.len() == priorities.len()
            && priorities.iter().all(|priority| {
                seed.priority_definitions
                    .get(*priority)
                    .is_some_and(|value| !value.is_empty())
            }),
        "Unknown or incomplete catalog priorities.",
    )?;
    for entry in &seed.entries {
        require(
            !entry.name.is_empty()
                && !entry.notes.is_empty()
                && !entry.sources.is_empty()
                && priorities.contains(&entry.priority.as_str())
                && [
                    "engine",
                    "engine-distribution",
                    "implementation",
                    "framework",
                    "vendor-stack",
                    "deployment-runtime",
                    "external-service",
                    "packaging",
                ]
                .contains(&entry.kind.as_str())
                && ["active", "maintenance", "archived", "unknown"]
                    .contains(&entry.upstream_lifecycle.as_str()),
            "Invalid runtime definition metadata.",
        )?;
        let candidates = unique(
            entry.candidate_backends.iter().map(String::as_str),
            "candidate compute path",
        )?;
        let workloads = unique(
            entry.tasks.iter().map(String::as_str),
            "runtime discovery task",
        )?;
        require(
            !candidates.is_empty() && candidates.is_subset(&backends),
            "Dangling candidate compute path.",
        )?;
        require(
            !workloads.is_empty() && workloads.is_subset(&tasks),
            "Dangling runtime discovery task.",
        )?;
    }
    unique(
        seed.native_recipe_migrations
            .iter()
            .map(|row| row.legacy_recipe.as_str()),
        "native recipe alias",
    )?;
    let mut mapped = BTreeSet::new();
    let mut support_count = 0;
    for migration in &seed.native_recipe_migrations {
        require(
            identifier(&migration.legacy_recipe),
            "Invalid native recipe alias.",
        )?;
        let parity_target = |id: &str| {
            seed.entries
                .iter()
                .any(|entry| entry.id == id && entry.priority == "parity")
        };
        require(
            parity_target(&migration.catalog_entry),
            "Dangling or non-parity native alias target.",
        )?;
        require(
            !entries.contains(migration.legacy_recipe.as_str())
                || migration.legacy_recipe == migration.catalog_entry,
            "Native alias collides with a different runtime identity.",
        )?;
        mapped.insert(migration.catalog_entry.as_str());
        let labels = unique(
            migration.native_backend_labels.iter().map(String::as_str),
            "native backend label",
        )?;
        require(
            !labels.is_empty()
                && !migration.display_name.is_empty()
                && !migration.migration_rule.is_empty(),
            "Incomplete native recipe alias.",
        )?;
        require(
            labels.iter().all(|id| {
                matches!(
                    *id,
                    "cpu"
                        | "cuda"
                        | "rocm"
                        | "vulkan"
                        | "metal"
                        | "system"
                        | "npu"
                        | "hybrid-rocm"
                        | "hrx"
                )
            }),
            "Unknown native backend label semantics.",
        )?;
        require(
            labels.iter().all(|id| match *id {
                "system" | "npu" | "hybrid-rocm" => true,
                "rocm" => backends.contains("hip"),
                id => backends.contains(id),
            }),
            "Native backend alias has a dangling compute path.",
        )?;
        let mut rows = BTreeSet::new();
        let mut supported_labels = BTreeSet::new();
        for row in &migration.native_support_rows {
            require(
                labels.contains(row.backend.as_str()),
                "Support row has an undeclared backend label.",
            )?;
            require(
                !row.tier.is_empty()
                    && !row.format.is_empty()
                    && !row.device_declaration.is_empty()
                    && !row.os.is_empty(),
                "Incomplete native support row.",
            )?;
            let systems = unique(row.os.iter().map(String::as_str), "native support OS")?;
            require(
                systems
                    .iter()
                    .all(|os| matches!(*os, "linux" | "windows" | "macos")),
                "Unknown native support OS.",
            )?;
            require(
                rows.insert((
                    row.backend.as_str(),
                    row.tier.as_str(),
                    row.format.as_str(),
                    systems,
                    row.device_declaration.as_str(),
                )),
                "Duplicate native support row.",
            )?;
            supported_labels.insert(row.backend.as_str());
            support_count += 1;
        }
        require(
            labels == supported_labels,
            "Native alias lacks an exact support row.",
        )?;
        unique(
            migration
                .profile_overrides
                .iter()
                .map(|profile| profile.legacy_backend.as_str()),
            "native profile override",
        )?;
        for profile in &migration.profile_overrides {
            require(
                labels.contains(profile.legacy_backend.as_str())
                    && parity_target(&profile.catalog_entry)
                    && !profile.reason.is_empty(),
                "Dangling or incomplete native profile override.",
            )?;
            mapped.insert(profile.catalog_entry.as_str());
        }
        if labels.contains("hybrid-rocm") {
            require(
                migration.legacy_recipe == "llamacpp"
                    && migration.profile_overrides.iter().any(|profile| {
                        profile.legacy_backend == "hybrid-rocm"
                            && profile.catalog_entry == "llama-xdna-hybrid"
                    }),
                "Hybrid native alias must preserve its distribution identity.",
            )?;
        }
    }
    require(
        seed.native_recipe_migrations.len() == seed.native_reference.recipe_count
            && support_count == seed.native_reference.support_row_count,
        "Native reference counts do not match the catalog.",
    )?;
    require(
        seed.entries
            .iter()
            .filter(|entry| entry.priority == "parity")
            .all(|entry| mapped.contains(entry.id.as_str())),
        "Parity runtime lacks a native migration alias.",
    )?;
    for family in &seed.model_family_examples {
        let candidates = unique(
            family.runtime_candidates.iter().map(String::as_str),
            "model family candidate",
        )?;
        require(
            !candidates.is_empty()
                && candidates.is_subset(&entries)
                && !family.notes.is_empty()
                && family.source.starts_with("https://"),
            "Dangling or incomplete model family example.",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn seed() -> Value {
        serde_json::from_str(SEED).unwrap()
    }

    fn rejects(mutator: impl FnOnce(&mut Value)) {
        let mut value = seed();
        mutator(&mut value);
        assert_eq!(
            Catalog::from_json(&value.to_string()).unwrap_err().code,
            "invalid_catalog_seed"
        );
    }

    #[test]
    fn seed_preserves_inventory_and_exact_native_rows() {
        let catalog = Catalog::from_seed().unwrap();
        let view = catalog.projection();
        assert_eq!(view.definitions.len(), 69);
        assert_eq!(view.native_recipe_migrations.len(), 18);
        let mut count = 0;
        for migration in view.native_recipe_migrations {
            for backend in &migration.native_backend_labels {
                let resolution = catalog
                    .resolve_legacy(&migration.legacy_recipe, backend)
                    .unwrap();
                let expected: Vec<_> = migration
                    .native_support_rows
                    .iter()
                    .filter(|row| &row.backend == backend)
                    .cloned()
                    .collect();
                assert_eq!(resolution.native_support_rows, expected);
                count += resolution.native_support_rows.len();
                assert!(!resolution.execution_enabled);
                assert!(resolution.qualification_receipts.is_empty());
            }
        }
        assert_eq!(count, 44);
        for (priority, count) in [
            ("parity", 19),
            ("first-expansion", 12),
            ("conditional", 27),
            ("attach", 6),
            ("reference-only", 3),
            ("legacy", 2),
        ] {
            assert_eq!(
                catalog
                    .definitions()
                    .iter()
                    .filter(|entry| entry.priority == priority)
                    .count(),
                count
            );
        }
    }

    #[test]
    fn exact_aliases_keep_implementations_distinct() {
        let catalog = Catalog::from_seed().unwrap();
        for (recipe, backend, implementation) in [
            ("asr4all", "cpu", "futo-transcribe-cpp"),
            ("kokoro", "cpu", "kokoros"),
            ("parakeet", "cpu", "parakeet-redux"),
            ("llamacpp", "rocm", "llama-cpp"),
            ("llamacpp", "hybrid-rocm", "llama-xdna-hybrid"),
        ] {
            assert_eq!(
                catalog
                    .resolve_legacy(recipe, backend)
                    .unwrap()
                    .implementation_id,
                implementation
            );
        }
        assert_ne!(
            catalog.definition("cactus-small").unwrap().id,
            catalog.definition("cactus-engine").unwrap().id
        );
        assert!(catalog.definition("handy-transcribe-cpp").is_some());
        let hybrid = catalog.resolve_legacy("llamacpp", "hybrid-rocm").unwrap();
        assert!(hybrid.override_reason.is_some());
        assert_eq!(hybrid.adapter.status, AdapterStatus::Planned);
        assert_eq!(hybrid.native_support_rows[0].os, ["linux"]);
    }

    #[test]
    fn labels_are_exact_and_candidates_never_create_support() {
        let catalog = Catalog::from_seed().unwrap();
        assert_eq!(
            catalog.resolve_legacy("llama-cpp", "cpu").unwrap_err().code,
            "unknown_legacy_recipe"
        );
        for backend in ["hip", "CPU", " cpu", "sycl", "openvino", "npu"] {
            assert_eq!(
                catalog
                    .resolve_legacy("llamacpp", backend)
                    .unwrap_err()
                    .code,
                "unknown_legacy_backend"
            );
        }
        assert!(
            catalog
                .definition("llama-cpp")
                .unwrap()
                .candidate_backends
                .iter()
                .any(|id| id == "sycl")
        );
        assert_eq!(
            catalog
                .resolve_legacy("llamacpp", "rocm")
                .unwrap()
                .compute_path_id
                .as_deref(),
            Some("hip")
        );
        assert!(catalog.resolve_legacy("kokoro", "cuda").is_err());
    }

    #[test]
    fn provenance_npu_and_hybrid_cannot_guess_a_compute_path() {
        let catalog = Catalog::from_seed().unwrap();
        for (recipe, backend, implementation, blocker) in [
            (
                "llamacpp",
                "system",
                "llama-cpp",
                ResolutionBlockerCode::InstallationProvenanceUnresolved,
            ),
            (
                "flm",
                "npu",
                "fastflowlm",
                ResolutionBlockerCode::RecipeScopedNpuStackRequired,
            ),
            (
                "ryzenai-llm",
                "npu",
                "ryzenai-oga",
                ResolutionBlockerCode::RecipeScopedNpuStackRequired,
            ),
            (
                "whispercpp",
                "npu",
                "whisper-cpp",
                ResolutionBlockerCode::RecipeScopedNpuStackRequired,
            ),
            (
                "llamacpp",
                "hybrid-rocm",
                "llama-xdna-hybrid",
                ResolutionBlockerCode::HybridPlacementRequired,
            ),
        ] {
            let result = catalog.resolve_legacy(recipe, backend).unwrap();
            assert_eq!(result.implementation_id, implementation);
            assert!(result.compute_path_id.is_none());
            assert!(result.blockers.iter().any(|reason| reason.code == blocker));
            assert!(
                result
                    .blockers
                    .iter()
                    .any(|reason| reason.code == ResolutionBlockerCode::BundleIdentityRequired)
            );
            assert!(
                result
                    .blockers
                    .iter()
                    .any(|reason| reason.code == ResolutionBlockerCode::DeviceIdentityRequired)
            );
        }
    }

    #[test]
    fn projections_and_resolution_remain_pure_and_unqualified() {
        let catalog = Catalog::from_seed().unwrap();
        let before = serde_json::to_value(catalog.projection()).unwrap();
        let first =
            serde_json::to_value(catalog.resolve_legacy("llamacpp", "cpu").unwrap()).unwrap();
        assert_eq!(
            first,
            serde_json::to_value(catalog.resolve_legacy("llamacpp", "cpu").unwrap()).unwrap()
        );
        assert_eq!(before, serde_json::to_value(catalog.projection()).unwrap());
        assert_eq!(before["qualification_receipts"], json!([]));
        assert_eq!(before["execution_enabled"], false);
        assert_eq!(
            catalog
                .projection()
                .definitions
                .iter()
                .filter(|entry| entry.adapter.status == AdapterStatus::Partial)
                .count(),
            2
        );
        for forbidden in [
            "executable",
            "api_key",
            "environment",
            "installed",
            "supported",
        ] {
            assert!(!before.as_object().unwrap().contains_key(forbidden));
            assert!(!first.as_object().unwrap().contains_key(forbidden));
        }
    }

    #[test]
    fn malformed_schema_collisions_and_dangling_references_fail() {
        assert!(Catalog::from_json("{").is_err());
        rejects(|value| value["schema_version"] = json!(2));
        rejects(|value| value["execution_enabled"] = json!(true));
        rejects(|value| value["executable"] = json!("/tmp/worker"));
        rejects(|value| value["entries"][1]["id"] = value["entries"][0]["id"].clone());
        rejects(|value| value["entries"][0]["candidate_backends"] = json!(["missing"]));
        rejects(|value| value["entries"][0]["tasks"] = json!(["missing"]));
        rejects(|value| value["native_recipe_migrations"][0]["catalog_entry"] = json!("missing"));
        rejects(|value| value["native_recipe_migrations"][1]["legacy_recipe"] = json!("acestep"));
        rejects(|value| value["native_recipe_migrations"][1]["legacy_recipe"] = json!("llama-cpp"));
        rejects(|value| {
            value["native_recipe_migrations"][0]["native_backend_labels"] = json!(["cuda", "cuda"])
        });
        rejects(|value| {
            value["native_recipe_migrations"][0]["native_support_rows"][0]["backend"] = json!("npu")
        });
        rejects(|value| value["native_reference"]["support_row_count"] = json!(45));
        rejects(|value| value["model_family_examples"][0]["id"] = json!("llama-cpp"));
        rejects(|value| {
            value["model_family_examples"][0]["runtime_candidates"] = json!(["missing"])
        });
        rejects(|value| value["native_recipe_migrations"][5]["profile_overrides"] = json!([]));
        rejects(|value| {
            let profile = value["native_recipe_migrations"][5]["profile_overrides"][0].clone();
            value["native_recipe_migrations"][5]["profile_overrides"]
                .as_array_mut()
                .unwrap()
                .push(profile);
        });
    }
}
