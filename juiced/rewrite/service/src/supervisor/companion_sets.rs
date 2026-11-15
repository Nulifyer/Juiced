//! Artifact sets that `models/add` composes from stored files: the parts of a split GGUF and
//! a llama.cpp vision projector.

use super::*;
use crate::{
    artifacts::{
        ArtifactError, ArtifactLease, ArtifactRole, ArtifactSetMember, ArtifactSetOrigin,
        ArtifactSetSpec, ArtifactStore,
    },
    engines::{models, split},
    runtime_arguments::valid_member_name,
};
use std::{fs::File, path::Path, sync::atomic::AtomicBool};

const IMPORTER_ID: &str = "juiced-models-add";
const IMPORTER_VERSION: &str = "juiced-models-add-1";
const SOURCE_REVISION: &str = "local";
const PROJECTOR_NAME: &str = "mmproj.gguf";

fn unprocessable(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
}

pub(super) fn part_missing(name: &str, part: u32, parts: u32) -> ApiError {
    unprocessable(
        "split_part_missing",
        format!(
            "Part {part} of {parts} ({name}) is missing. Keep every part of the split model in one folder."
        ),
    )
}

pub(super) fn part_mismatch(name: &str, part: u32, parts: u32) -> ApiError {
    unprocessable(
        "split_part_mismatch",
        format!("{name} is not part {part} of {parts} according to its GGUF header."),
    )
}

pub(super) fn name_invalid() -> ApiError {
    unprocessable(
        "split_name_invalid",
        "This GGUF is one part of a split model, but its file name lacks the -00001-of-0000N.gguf ending llama.cpp uses to find the other parts. Rename the parts to that pattern.",
    )
}

pub(super) fn parts_limit() -> ApiError {
    unprocessable(
        "split_parts_limit",
        format!(
            "A split model can have at most {} parts.",
            split::MAXIMUM_PARTS
        ),
    )
}

fn parts_required() -> ApiError {
    unprocessable(
        "split_parts_required",
        "This file is one part of a split GGUF. Add the model with parts listing every part.",
    )
}

fn parts_incomplete(parts: u64) -> ApiError {
    unprocessable(
        "split_parts_incomplete",
        format!("This split model has {parts} parts. Add all of them together."),
    )
}

pub(super) fn projector_invalid() -> ApiError {
    unprocessable(
        "projector_invalid",
        "The projector must be one vision projector GGUF (architecture clip), usually named mmproj.",
    )
}

pub(super) fn projector_needs_model() -> ApiError {
    unprocessable(
        "projector_needs_model",
        "This is a vision projector (mmproj). Add the language model and pass this file as its projector.",
    )
}

fn unreadable() -> ApiError {
    super::engines::format_error(models::FormatError::Unavailable)
}

fn reserve(store: &Arc<ArtifactStore>, digest: &str) -> Result<ArtifactLease, ApiError> {
    store.reserve_lease(digest).map_err(ApiError::from)
}

fn stored_name(store: &ArtifactStore, digest: &str, fallback: &str) -> String {
    store
        .source_file_name(digest)
        .filter(|name| valid_member_name(name) && name.ends_with(".gguf"))
        .unwrap_or_else(|| fallback.to_owned())
}

/// Orders the parts by their GGUF headers and publishes them, with an optional projector, as
/// one set whose member names follow the llama.cpp part spelling.
pub(super) fn split_set(
    store: &Arc<ArtifactStore>,
    parts: &[String],
    projector: Option<&str>,
) -> Result<String, ApiError> {
    let total = u32::try_from(parts.len()).unwrap_or(u32::MAX);
    if total < 2 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_parts",
            "parts lists the artifacts of every part of one split GGUF, at least two.",
        ));
    }
    if total > split::MAXIMUM_PARTS {
        return Err(parts_limit());
    }
    let mut ordered: Vec<Option<(String, u64)>> = vec![None; parts.len()];
    for digest in parts {
        let lease = reserve(store, digest)?;
        let mut file = File::open(lease.path()).map_err(|_| unreadable())?;
        let header = crate::profiles::read_gguf_split(&mut file).unwrap_or_default();
        match (header.number, header.count) {
            (_, Some(count)) if count > 1 && count != u64::from(total) => {
                return Err(parts_incomplete(count));
            }
            (Some(number), Some(_))
                if ordered
                    .get(number as usize)
                    .is_some_and(|slot| slot.is_none()) =>
            {
                ordered[number as usize] = Some((digest.clone(), lease.artifact().bytes));
            }
            _ => {
                return Err(unprocessable(
                    "split_part_mismatch",
                    format!("Artifact {digest} is not a distinct part of this split GGUF."),
                ));
            }
        }
    }
    let ordered: Vec<(String, u64)> = ordered.into_iter().flatten().collect();
    let first = store.source_file_name(&ordered[0].0);
    let prefix = first
        .as_deref()
        .and_then(split::parse)
        .map(|(prefix, ..)| prefix)
        .filter(|prefix| valid_member_name(&split::name(prefix, total, total)))
        .unwrap_or("model");
    let mut members: Vec<ArtifactSetMember> = ordered
        .into_iter()
        .enumerate()
        .map(|(index, (artifact, bytes))| ArtifactSetMember {
            name: split::name(prefix, index as u32 + 1, total),
            role: if index == 0 {
                ArtifactRole::Model
            } else {
                ArtifactRole::WeightShard
            },
            artifact,
            bytes,
        })
        .collect();
    if let Some(projector) = projector {
        let member = projector_member(store, projector, &members)?;
        members.push(member);
    }
    publish(store, members[0].name.clone(), members, None)
}

/// Adds a projector to one stored model file or to the set it already belongs to.
pub(super) fn with_projector(
    store: &Arc<ArtifactStore>,
    artifact: &str,
    set: Option<&str>,
    projector: &str,
) -> Result<String, ApiError> {
    let (primary, mut members, origin) = match set {
        Some(id) => {
            let view = store.artifact_set(id).map_err(ApiError::from)?;
            let specification = &view.specification;
            if specification
                .members
                .iter()
                .any(|member| member.role == ArtifactRole::Projector)
            {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "projector_exists",
                    "This artifact set already has a projector.",
                ));
            }
            (
                specification.primary.clone(),
                specification.members.clone(),
                Some(specification.origin.clone()),
            )
        }
        None => {
            let lease = reserve(store, artifact)?;
            let name = stored_name(store, artifact, "model.gguf");
            let member = ArtifactSetMember {
                name: name.clone(),
                role: ArtifactRole::Model,
                artifact: artifact.to_owned(),
                bytes: lease.artifact().bytes,
            };
            (name, vec![member], None)
        }
    };
    let member = projector_member(store, projector, &members)?;
    members.push(member);
    publish(store, primary, members, origin)
}

fn projector_member(
    store: &Arc<ArtifactStore>,
    digest: &str,
    members: &[ArtifactSetMember],
) -> Result<ArtifactSetMember, ApiError> {
    let lease = reserve(store, digest)?;
    let (format, _) = models::detect(lease.path()).map_err(super::engines::format_error)?;
    if !models::is_projector(&format) {
        return Err(projector_invalid());
    }
    let name = Some(stored_name(store, digest, PROJECTOR_NAME))
        .filter(|name| !members.iter().any(|member| member.name == *name))
        .unwrap_or_else(|| PROJECTOR_NAME.to_owned());
    Ok(ArtifactSetMember {
        name,
        role: ArtifactRole::Projector,
        artifact: digest.to_owned(),
        bytes: lease.artifact().bytes,
    })
}

fn publish(
    store: &Arc<ArtifactStore>,
    primary: String,
    members: Vec<ArtifactSetMember>,
    origin: Option<ArtifactSetOrigin>,
) -> Result<String, ApiError> {
    let specification = ArtifactSetSpec {
        schema_version: 1,
        origin: origin.unwrap_or_else(|| ArtifactSetOrigin {
            importer_id: IMPORTER_ID.into(),
            importer_version: IMPORTER_VERSION.into(),
            source_revision: SOURCE_REVISION.into(),
        }),
        primary,
        members,
    };
    let mut attempts = 0;
    loop {
        let operation = store.set_operation(Arc::new(AtomicBool::new(false)));
        match store.publish_set(specification.clone(), store.generation(), &operation) {
            Ok(view) => return Ok(view.id),
            // Another store write moved the generation between the read and the publish.
            Err(ArtifactError::Conflict) if attempts < 3 => attempts += 1,
            Err(error) => return Err(error.into()),
        }
    }
}

/// A stored GGUF that declares several parts must arrive with all of them, each under its
/// llama.cpp part name and with a header that agrees.
pub(super) fn verify_stored(
    primary: Option<&str>,
    path: &Path,
    companions: &[(&str, ArtifactRole, &Path)],
) -> Result<(), ApiError> {
    let shards: Vec<_> = companions
        .iter()
        .filter(|(_, role, _)| *role == ArtifactRole::WeightShard)
        .collect();
    let mut file = File::open(path).map_err(|_| unreadable())?;
    let declared = split::declared_parts(&mut file).map_err(|_| unreadable())?;
    let Some(count) = declared else {
        return if shards.is_empty() {
            Ok(())
        } else {
            Err(part_mismatch(primary.unwrap_or("The model file"), 1, 1))
        };
    };
    if shards.is_empty() {
        return Err(parts_required());
    }
    let (_, first, parts) = primary.and_then(split::parse).ok_or_else(name_invalid)?;
    if first != 1 || u64::from(parts) != count || shards.len() + 1 != parts as usize {
        return Err(parts_incomplete(count));
    }
    let name = primary.unwrap_or_default();
    split::verify(&mut file, 1, parts).map_err(|_| part_mismatch(name, 1, parts))?;
    for (name, _, path) in shards {
        match split::parse(name) {
            Some((_, part, total)) if total == parts => {
                split::verify_path(path, part, parts)
                    .map_err(|_| part_mismatch(name, part, parts))?;
            }
            _ => return Err(parts_incomplete(count)),
        }
    }
    Ok(())
}
