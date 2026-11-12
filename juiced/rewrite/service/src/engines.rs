pub mod archive;
pub mod catalog;
pub mod download;
pub mod models;
#[cfg(test)]
mod speech_engine_tests;
#[cfg(test)]
mod tests;

use crate::hardware::{Availability, HostReport, Inventory};
use catalog::{Build, Engine};
use serde::Serialize;
use serde_json::Value;

pub const GITHUB: &str = "https://github.com";
pub const GITHUB_API: &str = "https://api.github.com";

#[derive(Clone, Debug)]
pub struct ReleaseEndpoints {
    pub downloads: String,
    pub api: String,
    pub pinned_digests: bool,
}

impl ReleaseEndpoints {
    pub fn from_environment() -> Self {
        #[cfg(feature = "test-fixture")]
        if let Some(base) = std::env::var_os("JUICED_TEST_ENGINE_RELEASES")
            .and_then(|value| value.into_string().ok())
        {
            let base = base.trim_end_matches('/').to_owned();
            return Self {
                api: format!("{base}/api"),
                downloads: base,
                pinned_digests: false,
            };
        }
        Self {
            downloads: GITHUB.into(),
            api: GITHUB_API.into(),
            pinned_digests: true,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Installed {
    pub package: String,
    pub engine: Option<Engine>,
    pub backend: Option<String>,
    pub gpu_target: Option<String>,
    pub tag: Option<String>,
    pub installed: bool,
    pub declared_host_match: bool,
}

pub fn installed_packages(inventory: &Value) -> Vec<Installed> {
    inventory["packages"]
        .as_array()
        .map(|packages| {
            packages
                .iter()
                .filter_map(|package| {
                    let release = &package["provenance"]["release"];
                    let text = |value: &Value| value.as_str().map(str::to_owned);
                    Some(Installed {
                        package: package["id"].as_str()?.to_owned(),
                        engine: package["implementation_id"]
                            .as_str()
                            .and_then(Engine::parse),
                        backend: text(&release["backend"]),
                        gpu_target: text(&release["gpu_target"]),
                        tag: text(&release["tag"]),
                        installed: package["phase"] == "installed",
                        declared_host_match: package["declared_host_match"] == true,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChoiceError {
    NoUsableBackend,
    NotSelectable(String),
    NotInstalled {
        backend: String,
        gpu_target: Option<String>,
        installable: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub backend: String,
    pub package: String,
    pub gpu: bool,
    pub reason: String,
}

fn selectable(inventory: &Inventory, engine: Engine, backend: &str) -> bool {
    inventory.backends.iter().any(|variant| {
        variant.recipe == engine.recipe()
            && variant.backend == backend
            && (variant.tier == "core"
                || (variant.tier == "experimental" && engine.accepts_experimental()))
            && variant.backend != "system"
            && !variant.backend.starts_with("hybrid")
            && variant.state != Availability::Unavailable
    })
}

fn fallback_backend(inventory: &Inventory, engine: Engine) -> Option<String> {
    if !engine.accepts_experimental() {
        return None;
    }
    inventory
        .backends
        .iter()
        .find(|variant| {
            variant.recipe == engine.recipe()
                && variant.state == Availability::Available
                && selectable(inventory, engine, &variant.backend)
        })
        .map(|variant| variant.backend.clone())
}

/// Uses the host recommendation unless the operator named a backend; never substitutes a
/// lower-ranked backend because the recommended one is missing.
pub fn choose(
    engine: Engine,
    inventory: &Inventory,
    host: &HostReport,
    installed: &[Installed],
    requested: Option<&str>,
) -> Result<Choice, ChoiceError> {
    let recommended = inventory
        .recommendations
        .get(engine.recipe())
        .cloned()
        .flatten();
    let backend = match requested {
        Some(backend) if selectable(inventory, engine, backend) => backend.to_owned(),
        Some(backend) => return Err(ChoiceError::NotSelectable(backend.to_owned())),
        None => recommended
            .clone()
            .or_else(|| fallback_backend(inventory, engine))
            .ok_or(ChoiceError::NoUsableBackend)?,
    };
    let build = catalog::resolve(engine, &backend, host).ok();
    let candidates: Vec<_> = installed
        .iter()
        .filter(|package| {
            package.installed
                && package.declared_host_match
                && package.engine == Some(engine)
                && package.backend.as_deref() == Some(backend.as_str())
                && build.is_none_or(|build| package.gpu_target.as_deref() == build.gpu_target)
        })
        .collect();
    let selected = candidates
        .iter()
        .rev()
        .find(|package| build.is_some_and(|build| package.tag.as_deref() == Some(build.tag)))
        .or(candidates.last())
        .ok_or_else(|| ChoiceError::NotInstalled {
            backend: backend.clone(),
            gpu_target: build.and_then(|build| build.gpu_target.map(str::to_owned)),
            installable: build.is_some(),
        })?;
    let target = selected
        .gpu_target
        .as_deref()
        .map(|target| format!(" for {target}"))
        .unwrap_or_default();
    let basis = if requested.is_some() {
        "was requested"
    } else if recommended.is_some() {
        "is the recommended"
    } else {
        "is the only available"
    };
    Ok(Choice {
        reason: format!(
            "{backend} {basis} {} backend on this host; using installed package {}{target}{}.",
            engine.name(),
            selected.package,
            selected
                .tag
                .as_deref()
                .map(|tag| format!(" ({tag})"))
                .unwrap_or_default()
        ),
        gpu: backend != "cpu",
        backend,
        package: selected.package.clone(),
    })
}

#[derive(Serialize)]
pub struct Installable {
    pub engine: Engine,
    pub backend: &'static str,
    pub build: Option<Build>,
    pub unresolved: Option<catalog::Unresolved>,
    pub availability: Option<Availability>,
    pub reasons: Vec<crate::hardware::Reason>,
    pub recommended: bool,
    pub installed_package: Option<String>,
}

pub fn installable(
    inventory: &Inventory,
    host: &HostReport,
    installed: &[Installed],
) -> Vec<Installable> {
    let mut seen = Vec::new();
    for build in catalog::BUILDS {
        if !seen.contains(&(build.engine, build.backend)) {
            seen.push((build.engine, build.backend));
        }
    }
    seen.extend(catalog::UNPUBLISHED.iter().copied());
    seen.into_iter()
        .map(|(engine, backend)| {
            let resolved = catalog::resolve(engine, backend, host);
            let variant = inventory
                .backends
                .iter()
                .find(|variant| variant.recipe == engine.recipe() && variant.backend == backend);
            let build = resolved.ok();
            Installable {
                engine,
                backend,
                build,
                unresolved: resolved.err(),
                availability: variant.map(|variant| variant.state),
                reasons: variant
                    .map(|variant| variant.reasons.clone())
                    .unwrap_or_default(),
                recommended: variant.is_some_and(|variant| variant.recommended),
                installed_package: installed
                    .iter()
                    .rev()
                    .find(|package| {
                        package.installed
                            && package.engine == Some(engine)
                            && package.backend.as_deref() == Some(backend)
                            && build.is_none_or(|build| {
                                package.gpu_target.as_deref() == build.gpu_target
                                    && package.tag.as_deref() == Some(build.tag)
                            })
                            && (build.is_some()
                                || catalog::UNPUBLISHED.contains(&(engine, backend)))
                    })
                    .map(|package| package.package.clone()),
            }
        })
        .collect()
}
