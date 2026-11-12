use crate::native_settings::{MAX_DOCUMENT_BYTES, Role};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use std::{collections::BTreeMap, fmt};

pub const STORE_ROOT: &str = ".juiced-native-settings";
pub const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
pub const MAX_GENERATION_BYTES: u64 = 64 * 1024 * 1024;
const INVALID: &str = "Native settings descriptor is invalid; preserve the settings store.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    DescriptorTooLarge,
    InvalidDescriptor,
    InvalidId,
    InvalidDigest,
    InvalidBlob,
    CohortTooLarge,
    AliasAuthorityChanged,
    ConflictingGeneration,
    ConflictingBlob,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(INVALID)
    }
}
impl std::error::Error for Error {}

fn wire<'de, T: Deserialize<'de>, D: Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
    T::deserialize(deserializer).map_err(|_| D::Error::custom(INVALID))
}
fn lower_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Id(String);
impl Id {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if !lower_hex(&value, 32) {
            return Err(Error::InvalidId);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Id { .. }")
    }
}
impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(wire::<String, D>(deserializer)?).map_err(|_| D::Error::custom(INVALID))
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Digest(String);
impl Digest {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if !lower_hex(&value, 64) {
            return Err(Error::InvalidDigest);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Digest { .. }")
    }
}
impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(wire::<String, D>(deserializer)?).map_err(|_| D::Error::custom(INVALID))
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Blob {
    id: Id,
    bytes: u64,
    sha256: Digest,
    permissions: u32,
}
impl fmt::Debug for Blob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Blob { .. }")
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlobWire {
    id: Id,
    bytes: u64,
    sha256: Digest,
    permissions: u32,
}
impl Blob {
    pub fn new(id: Id, bytes: u64, sha256: Digest, permissions: u32) -> Result<Self, Error> {
        if bytes > MAX_DOCUMENT_BYTES as u64 || permissions > 0o7777 {
            return Err(Error::InvalidBlob);
        }
        Ok(Self {
            id,
            bytes,
            sha256,
            permissions,
        })
    }
    pub fn id(&self) -> &Id {
        &self.id
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn sha256(&self) -> &Digest {
        &self.sha256
    }
    pub fn permissions(&self) -> u32 {
        self.permissions
    }
}
impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = wire::<BlobWire, D>(deserializer)?;
        Self::new(value.id, value.bytes, value.sha256, value.permissions)
            .map_err(|_| D::Error::custom(INVALID))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Presence {
    Absent,
    Blob(Blob),
}
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum PresenceWire {
    Absent {},
    Blob(Blob),
}
impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match wire::<PresenceWire, D>(deserializer)? {
            PresenceWire::Absent {} => Self::Absent,
            PresenceWire::Blob(blob) => Self::Blob(blob),
        })
    }
}
impl Presence {
    pub fn blob(&self) -> Option<&Blob> {
        match self {
            Self::Absent => None,
            Self::Blob(blob) => Some(blob),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Aliases {
    RetainedAtSource,
    Imported { presence: Presence },
}
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum AliasesWire {
    RetainedAtSource {},
    Imported { presence: Presence },
}
impl<'de> Deserialize<'de> for Aliases {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match wire::<AliasesWire, D>(deserializer)? {
            AliasesWire::RetainedAtSource {} => Self::RetainedAtSource,
            AliasesWire::Imported { presence } => Self::Imported { presence },
        })
    }
}
impl Aliases {
    pub fn is_imported(&self) -> bool {
        matches!(self, Self::Imported { .. })
    }
    pub fn presence(&self) -> Option<&Presence> {
        match self {
            Self::RetainedAtSource => None,
            Self::Imported { presence } => Some(presence),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Files {
    config: Presence,
    jobs: Presence,
    recipe_options: Presence,
    user_models: Presence,
    execution_profiles: Presence,
    native_helpers: Presence,
    aliases: Aliases,
}
impl fmt::Debug for Files {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Files { .. }")
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesWire {
    config: Presence,
    jobs: Presence,
    recipe_options: Presence,
    user_models: Presence,
    execution_profiles: Presence,
    native_helpers: Presence,
    aliases: Aliases,
}
impl Files {
    pub fn new(
        config: Presence,
        jobs: Presence,
        recipe_options: Presence,
        user_models: Presence,
        execution_profiles: Presence,
        native_helpers: Presence,
        aliases: Aliases,
    ) -> Result<Self, Error> {
        let files = Self {
            config,
            jobs,
            recipe_options,
            user_models,
            execution_profiles,
            native_helpers,
            aliases,
        };
        let mut bytes = 0u64;
        for blob in files.blobs() {
            bytes = bytes
                .checked_add(blob.bytes)
                .filter(|bytes| *bytes <= MAX_GENERATION_BYTES)
                .ok_or(Error::CohortTooLarge)?;
        }
        consistent_blobs(files.blobs())?;
        Ok(files)
    }
    pub fn get(&self, role: Role) -> Option<&Presence> {
        match role {
            Role::Config => Some(&self.config),
            Role::Jobs => Some(&self.jobs),
            Role::RecipeOptions => Some(&self.recipe_options),
            Role::UserModels => Some(&self.user_models),
            Role::ExecutionProfiles => Some(&self.execution_profiles),
            Role::NativeHelpers => Some(&self.native_helpers),
            Role::Aliases => self.aliases.presence(),
        }
    }
    pub fn aliases(&self) -> &Aliases {
        &self.aliases
    }
    pub fn total_bytes(&self) -> u64 {
        self.blobs().map(Blob::bytes).sum()
    }
    fn blobs(&self) -> impl Iterator<Item = &Blob> {
        [
            &self.config,
            &self.jobs,
            &self.recipe_options,
            &self.user_models,
            &self.execution_profiles,
            &self.native_helpers,
        ]
        .into_iter()
        .chain(self.aliases.presence())
        .filter_map(Presence::blob)
    }
}
impl<'de> Deserialize<'de> for Files {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = wire::<FilesWire, D>(deserializer)?;
        Self::new(
            value.config,
            value.jobs,
            value.recipe_options,
            value.user_models,
            value.execution_profiles,
            value.native_helpers,
            value.aliases,
        )
        .map_err(|_| D::Error::custom(INVALID))
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Generation {
    id: Id,
    files: Files,
}
impl fmt::Debug for Generation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Generation { .. }")
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationWire {
    id: Id,
    files: Files,
}
impl Generation {
    pub fn new(id: Id, files: Files) -> Self {
        Self { id, files }
    }
    pub fn id(&self) -> &Id {
        &self.id
    }
    pub fn files(&self) -> &Files {
        &self.files
    }
}
impl<'de> Deserialize<'de> for Generation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = wire::<GenerationWire, D>(deserializer)?;
        Ok(Self::new(value.id, value.files))
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Binding {
    schema_version: u32,
    store_id: Id,
    original: Generation,
    current: Generation,
}
impl fmt::Debug for Binding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Binding { schema_version: 1, .. }")
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingWire {
    schema_version: u32,
    store_id: Id,
    original: Generation,
    current: Generation,
}
impl Binding {
    pub fn new(store_id: Id, original: Generation, current: Generation) -> Result<Self, Error> {
        if original.files.aliases.is_imported() != current.files.aliases.is_imported() {
            return Err(Error::AliasAuthorityChanged);
        }
        if original.id == current.id && original.files != current.files {
            return Err(Error::ConflictingGeneration);
        }
        consistent_blobs(original.files.blobs().chain(current.files.blobs()))?;
        Ok(Self {
            schema_version: 1,
            store_id,
            original,
            current,
        })
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_DESCRIPTOR_BYTES {
            return Err(Error::DescriptorTooLarge);
        }
        serde_json::from_slice(bytes).map_err(|_| Error::InvalidDescriptor)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = serde_json::to_vec(self).map_err(|_| Error::InvalidDescriptor)?;
        if bytes.len() > MAX_DESCRIPTOR_BYTES {
            return Err(Error::DescriptorTooLarge);
        }
        Ok(bytes)
    }
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
    pub fn store_id(&self) -> &Id {
        &self.store_id
    }
    pub fn original(&self) -> &Generation {
        &self.original
    }
    pub fn current(&self) -> &Generation {
        &self.current
    }
}
impl<'de> Deserialize<'de> for Binding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = wire::<BindingWire, D>(deserializer)?;
        if value.schema_version != 1 {
            return Err(D::Error::custom(INVALID));
        }
        Self::new(value.store_id, value.original, value.current)
            .map_err(|_| D::Error::custom(INVALID))
    }
}
fn consistent_blobs<'a>(blobs: impl Iterator<Item = &'a Blob>) -> Result<(), Error> {
    let mut seen = BTreeMap::new();
    for blob in blobs {
        if let Some(previous) = seen.insert(&blob.id, blob)
            && previous != blob
        {
            return Err(Error::ConflictingBlob);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::error::Error as _;

    fn id(number: u8) -> Id {
        Id::new(format!("{number:032x}")).unwrap()
    }
    fn blob(number: u8, bytes: u64) -> Presence {
        Presence::Blob(
            Blob::new(
                id(number),
                bytes,
                Digest::new("a".repeat(64)).unwrap(),
                0o600,
            )
            .unwrap(),
        )
    }
    fn files(aliases: Aliases) -> Files {
        Files::new(
            blob(1, 123),
            Presence::Absent,
            blob(2, 456),
            Presence::Absent,
            Presence::Absent,
            Presence::Absent,
            aliases,
        )
        .unwrap()
    }
    fn binding() -> Binding {
        let generation = Generation::new(id(20), files(Aliases::RetainedAtSource));
        Binding::new(id(30), generation.clone(), generation).unwrap()
    }
    fn reject(value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(Binding::decode(&bytes).is_err());
        assert!(serde_json::from_slice::<Binding>(&bytes).is_err());
    }

    #[test]
    fn fixed_wire_roundtrip_preserves_explicit_absence_and_shared_original_current_references() {
        let binding = binding();
        let bytes = binding.to_bytes().unwrap();
        assert_eq!(Binding::decode(&bytes).unwrap(), binding);
        assert_eq!(serde_json::from_slice::<Binding>(&bytes).unwrap(), binding);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["current"]["files"]["jobs"], json!({"state":"absent"}));
        assert_eq!(value["current"]["files"]["config"]["state"], "blob");
        assert_eq!(
            value["current"]["files"]["aliases"],
            json!({"state":"retained_at_source"})
        );
        assert_eq!(binding.current().files().total_bytes(), 579);
        assert_eq!(binding.original(), binding.current());
        assert_eq!(binding.schema_version(), 1);
        assert_eq!(binding.store_id().as_str(), format!("{:032x}", 30));
        assert_eq!(STORE_ROOT, ".juiced-native-settings");
        let files = files(Aliases::Imported {
            presence: Presence::Absent,
        });
        assert_eq!(files.get(Role::Aliases), Some(&Presence::Absent));
        let generation = Generation::new(id(20), files);
        let imported = Binding::new(id(30), generation.clone(), generation).unwrap();
        assert_eq!(
            Binding::decode(&imported.to_bytes().unwrap()).unwrap(),
            imported
        );
    }

    #[test]
    fn required_fields_schemas_and_unknown_fields_reject_under_direct_and_nested_serde() {
        let source = serde_json::to_value(binding()).unwrap();
        for version in [json!(0), json!(2), json!(1.0), json!("1"), Value::Null] {
            let mut value = source.clone();
            value["schema_version"] = version;
            reject(value);
        }
        for field in ["schema_version", "store_id", "original", "current"] {
            let mut value = source.clone();
            value.as_object_mut().unwrap().remove(field);
            reject(value);
        }
        for field in [
            "config",
            "jobs",
            "recipe_options",
            "user_models",
            "execution_profiles",
            "native_helpers",
            "aliases",
        ] {
            let mut value = source.clone();
            value["original"]["files"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            reject(value);
        }
        for path in [
            "binding",
            "generation",
            "files",
            "blob",
            "absent",
            "aliases",
        ] {
            let mut value = source.clone();
            let object = match path {
                "binding" => &mut value,
                "generation" => &mut value["original"],
                "files" => &mut value["original"]["files"],
                "blob" => &mut value["original"]["files"]["config"],
                "absent" => &mut value["original"]["files"]["jobs"],
                _ => &mut value["original"]["files"]["aliases"],
            };
            object["path"] = json!("../foreign");
            reject(value);
        }
        #[derive(Deserialize)]
        struct Nested {
            binding: Binding,
        }
        let nested: Nested = serde_json::from_value(json!({"binding":source.clone()})).unwrap();
        assert_eq!(nested.binding, binding());
        let mut bad = source;
        bad["current"]["files"]["config"]["permissions"] = json!(0o10000);
        assert!(serde_json::from_value::<Nested>(json!({"binding":bad})).is_err());
    }

    #[test]
    fn duplicate_fields_at_every_layer_are_rejected() {
        let source = String::from_utf8(binding().to_bytes().unwrap()).unwrap();
        for (needle, replacement) in [
            (
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1".to_owned(),
            ),
            (
                "\"store_id\":",
                format!("\"store_id\":\"{}\",\"store_id\":", "a".repeat(32)),
            ),
            ("\"id\":", format!("\"id\":\"{}\",\"id\":", "a".repeat(32))),
            (
                "\"jobs\":",
                "\"jobs\":{\"state\":\"absent\"},\"jobs\":".to_owned(),
            ),
            (
                "\"state\":\"blob\"",
                "\"state\":\"blob\",\"state\":\"blob\"".to_owned(),
            ),
            ("\"bytes\":123", "\"bytes\":123,\"bytes\":123".to_owned()),
            (
                "\"state\":\"absent\"",
                "\"state\":\"absent\",\"state\":\"absent\"".to_owned(),
            ),
        ] {
            let duplicated = source.replacen(needle, &replacement, 1);
            assert_ne!(duplicated, source);
            assert!(Binding::decode(duplicated.as_bytes()).is_err());
        }
        for bytes in [
            br#"{"state":"imported","presence":{"state":"absent"},"presence":{"state":"absent"}}"#
                .as_slice(),
            br#"{"state":"retained_at_source","state":"retained_at_source"}"#,
        ] {
            assert!(serde_json::from_slice::<Aliases>(bytes).is_err());
        }
    }

    #[test]
    fn opaque_ids_and_digests_cannot_supply_paths_or_other_text() {
        for invalid in [
            "",
            "..",
            "../foreign",
            "/absolute",
            "C:\\foreign",
            &"A".repeat(32),
            &"0".repeat(31),
            &"0".repeat(33),
            &"g".repeat(32),
        ] {
            assert_eq!(Id::new(invalid).unwrap_err(), Error::InvalidId);
            let mut value = serde_json::to_value(binding()).unwrap();
            value["store_id"] = json!(invalid);
            reject(value);
        }
        for invalid in [
            "",
            "../foreign",
            &"A".repeat(64),
            &"0".repeat(63),
            &"0".repeat(65),
            &"z".repeat(64),
        ] {
            assert_eq!(Digest::new(invalid).unwrap_err(), Error::InvalidDigest);
            let mut value = serde_json::to_value(binding()).unwrap();
            value["original"]["files"]["config"]["sha256"] = json!(invalid);
            reject(value);
        }
        assert_eq!(
            Id::new("0123456789abcdef0123456789abcdef")
                .unwrap()
                .as_str(),
            "0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn file_and_generation_sizes_are_independently_bounded_including_aliases() {
        let maximum = MAX_DOCUMENT_BYTES as u64;
        let exact = Files::new(
            blob(1, maximum),
            blob(2, maximum),
            blob(3, maximum),
            blob(4, maximum),
            Presence::Absent,
            Presence::Absent,
            Aliases::RetainedAtSource,
        )
        .unwrap();
        assert_eq!(exact.total_bytes(), MAX_GENERATION_BYTES);
        let original = Generation::new(id(20), exact.clone());
        let current = Generation::new(id(21), exact);
        let both = Binding::new(id(30), original, current).unwrap();
        assert_eq!(Binding::decode(&both.to_bytes().unwrap()).unwrap(), both);
        assert_eq!(
            Files::new(
                blob(1, maximum),
                blob(2, maximum),
                blob(3, maximum),
                blob(4, maximum),
                Presence::Absent,
                Presence::Absent,
                Aliases::Imported {
                    presence: blob(5, 1)
                }
            )
            .unwrap_err(),
            Error::CohortTooLarge
        );
        assert_eq!(
            Blob::new(
                id(1),
                maximum + 1,
                Digest::new("a".repeat(64)).unwrap(),
                0o600
            )
            .unwrap_err(),
            Error::InvalidBlob
        );
        assert_eq!(
            Blob::new(id(1), u64::MAX, Digest::new("a".repeat(64)).unwrap(), 0o600).unwrap_err(),
            Error::InvalidBlob
        );
        for field in ["bytes", "permissions"] {
            for number in [json!(-1), json!(u64::MAX), json!(1.0), json!("1")] {
                let mut value = serde_json::to_value(binding()).unwrap();
                value["original"]["files"]["config"][field] = number;
                reject(value);
            }
        }
        let mut value = serde_json::to_value(&both).unwrap();
        value["current"]["files"]["native_helpers"] = serde_json::to_value(blob(5, 1)).unwrap();
        reject(value);
        Blob::new(id(1), 0, Digest::new("a".repeat(64)).unwrap(), 0o7777).unwrap();
    }

    #[test]
    fn alias_authority_and_immutable_references_cannot_conflict() {
        let original = Generation::new(id(20), files(Aliases::RetainedAtSource));
        let imported = Generation::new(
            id(21),
            files(Aliases::Imported {
                presence: Presence::Absent,
            }),
        );
        assert_eq!(
            Binding::new(id(30), original.clone(), imported).unwrap_err(),
            Error::AliasAuthorityChanged
        );
        let mut value = serde_json::to_value(binding()).unwrap();
        value["current"]["files"]["aliases"] =
            json!({"state":"imported","presence":{"state":"absent"}});
        reject(value);
        let changed = Files::new(
            blob(1, 124),
            Presence::Absent,
            blob(2, 456),
            Presence::Absent,
            Presence::Absent,
            Presence::Absent,
            Aliases::RetainedAtSource,
        )
        .unwrap();
        assert_eq!(
            Binding::new(
                id(30),
                original.clone(),
                Generation::new(id(20), changed.clone())
            )
            .unwrap_err(),
            Error::ConflictingGeneration
        );
        assert_eq!(
            Binding::new(id(30), original, Generation::new(id(21), changed)).unwrap_err(),
            Error::ConflictingBlob
        );
        assert_eq!(
            Files::new(
                blob(1, 1),
                blob(1, 2),
                Presence::Absent,
                Presence::Absent,
                Presence::Absent,
                Presence::Absent,
                Aliases::RetainedAtSource
            )
            .unwrap_err(),
            Error::ConflictingBlob
        );
        let reused = blob(1, 123);
        let files = Files::new(
            reused.clone(),
            reused,
            Presence::Absent,
            Presence::Absent,
            Presence::Absent,
            Presence::Absent,
            Aliases::RetainedAtSource,
        )
        .unwrap();
        assert_eq!(files.total_bytes(), 246);
    }

    #[test]
    fn descriptor_byte_limit_is_checked_before_decode_and_diagnostics_are_redacted() {
        let mut exact = binding().to_bytes().unwrap();
        exact.resize(MAX_DESCRIPTOR_BYTES, b' ');
        Binding::decode(&exact).unwrap();
        exact.push(b' ');
        assert_eq!(
            Binding::decode(&exact).unwrap_err(),
            Error::DescriptorTooLarge
        );
        assert_eq!(
            Binding::decode(&vec![b'x'; MAX_DESCRIPTOR_BYTES + 1]).unwrap_err(),
            Error::DescriptorTooLarge
        );
        let marker = "JUICED_SYNTHETIC_PRIVATE_DESCRIPTOR_MARKER";
        for input in [
            format!(r#"{{"{marker}":"{marker}"}}"#),
            format!(r#"{{"schema_version":"{marker}"}}"#),
            format!(r#""{marker}""#),
        ] {
            let error = Binding::decode(input.as_bytes()).unwrap_err();
            assert!(!format!("{error:?} {error}").contains(marker));
            assert!(error.source().is_none());
            let error = serde_json::from_str::<Binding>(&input).unwrap_err();
            assert!(!format!("{error:?} {error}").contains(marker));
        }
        for input in [
            format!(r#"{{"state":"{marker}"}}"#),
            format!(r#"{{"state":"absent","{marker}":"{marker}"}}"#),
        ] {
            let error = serde_json::from_str::<Presence>(&input).unwrap_err();
            assert!(!format!("{error:?} {error}").contains(marker));
        }
        assert!(!format!("{:?}", binding()).contains(&"a".repeat(64)));
    }
}
