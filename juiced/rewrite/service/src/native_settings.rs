use serde::{Serialize, Serializer};
use serde_json::value::RawValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Write,
    sync::Arc,
};

pub const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_DEPTH: usize = 128;
pub const MAX_EVENTS: usize = 100_000;

type Object = BTreeMap<String, Arc<RawValue>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Config,
    Jobs,
    NativeHelpers,
    Aliases,
    RecipeOptions,
    ExecutionProfiles,
    UserModels,
}

impl Role {
    pub fn filename(self) -> &'static str {
        match self {
            Self::Config => "config.json",
            Self::Jobs => "jobs.json",
            Self::NativeHelpers => "native_helpers.json",
            Self::Aliases => "aliases.json",
            Self::RecipeOptions => "recipe_options.json",
            Self::ExecutionProfiles => "execution_profiles.json",
            Self::UserModels => "user_models.json",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    FileTooLarge,
    InvalidJson,
    DuplicateKey,
    DepthLimit,
    EventLimit,
    NotObject,
    InvalidRecognizedRow,
    InvalidOwnedSettings,
    InvalidRecognition,
    ConflictingRows,
    UnknownModel,
    InvalidSelection,
    RecipeChanged,
    WrongDocumentRole,
    CurrentViewMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub role: Option<Role>,
    pub reason: Reason,
}

impl Error {
    fn new(role: Option<Role>, reason: Reason) -> Self {
        Self { role, reason }
    }

    pub fn code(&self) -> &'static str {
        "model_settings_recovery_required"
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "Stored native model settings require recovery; preserve the supplied documents.",
        )
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Document {
    role: Role,
    original: Option<Arc<Vec<u8>>>,
    present: bool,
    dirty: bool,
    rows: Arc<Object>,
}

impl fmt::Debug for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Document")
            .field("role", &self.role)
            .field("present", &self.present)
            .finish_non_exhaustive()
    }
}

impl Document {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &RawValue)> {
        self.rows
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_ref()))
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn same_view(&self, other: &Self) -> bool {
        self.role == other.role
            && self.present == other.present
            && Arc::ptr_eq(&self.rows, &other.rows)
    }

    pub fn parse(role: Role, bytes: Option<Arc<Vec<u8>>>) -> Result<Self, Error> {
        let rows = match &bytes {
            Some(bytes) => parse_object(bytes, Some(role))?,
            None => Object::new(),
        };
        Ok(Self {
            role,
            present: bytes.is_some(),
            dirty: false,
            original: bytes,
            rows: Arc::new(rows),
        })
    }

    pub fn parse_bytes(role: Role, bytes: Option<&[u8]>) -> Result<Self, Error> {
        if bytes.is_some_and(|bytes| bytes.len() > MAX_DOCUMENT_BYTES) {
            return Err(Error::new(Some(role), Reason::FileTooLarge));
        }
        Self::parse(role, bytes.map(|bytes| Arc::new(bytes.to_vec())))
    }

    pub fn role(&self) -> Role {
        self.role
    }
    pub fn is_absent(&self) -> bool {
        !self.present
    }
    pub fn original_bytes(&self) -> Option<&Arc<Vec<u8>>> {
        self.original.as_ref()
    }
    pub fn row(&self, id: &str) -> Option<&RawValue> {
        self.rows.get(id).map(Arc::as_ref)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        if !self.dirty
            && let Some(original) = &self.original
        {
            return Ok(original.to_vec());
        }
        encode(&self.rows, Some(self.role))
    }

    pub(crate) fn rebase_current(&self, bytes: Option<Arc<Vec<u8>>>) -> Result<Self, Error> {
        let mismatch = || Error::new(Some(self.role), Reason::CurrentViewMismatch);
        if bytes
            .as_ref()
            .is_some_and(|bytes| bytes.len() > MAX_DOCUMENT_BYTES)
        {
            return Err(Error::new(Some(self.role), Reason::FileTooLarge));
        }
        match (self.present, bytes.as_ref()) {
            (false, None) => {}
            (true, Some(current)) => {
                if self.dirty {
                    let mut expected = ExpectedBytes {
                        bytes: current,
                        at: 0,
                    };
                    serde_json::to_writer(&mut expected, &self.rows).map_err(|_| mismatch())?;
                    if expected.at != current.len() {
                        return Err(mismatch());
                    }
                } else if !self.original.as_ref().is_some_and(|original| {
                    Arc::ptr_eq(original, current) || original.as_slice() == current.as_slice()
                }) {
                    return Err(mismatch());
                }
            }
            _ => return Err(mismatch()),
        }
        Ok(Self {
            role: self.role,
            original: bytes,
            present: self.present,
            dirty: false,
            rows: self.rows.clone(),
        })
    }

    fn changed(&self, rows: Object) -> Result<Self, Error> {
        let bytes = encode(&rows, Some(self.role))?;
        scan(&bytes, Some(self.role))?;
        Ok(Self {
            role: self.role,
            original: self.original.clone(),
            present: true,
            dirty: true,
            rows: Arc::new(rows),
        })
    }
}

#[derive(Clone, Default)]
pub struct SavedOptions(Object);

impl fmt::Debug for SavedOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SavedOptions { .. }")
    }
}

impl Serialize for SavedOptions {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl SavedOptions {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &RawValue)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_ref()))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        parse_object(bytes, None).map(Self)
    }
    pub fn get(&self, key: &str) -> Option<&RawValue> {
        self.0.get(key).map(Arc::as_ref)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(&self.0, None)
    }
}

#[derive(Clone)]
pub struct RecognizedRows {
    canonical: BTreeSet<String>,
    aliases: BTreeMap<String, String>,
}

impl fmt::Debug for RecognizedRows {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RecognizedRows { .. }")
    }
}

impl RecognizedRows {
    pub fn new(
        canonical: impl IntoIterator<Item = String>,
        aliases: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, Error> {
        let mut ids = BTreeSet::new();
        for id in canonical {
            if id.is_empty() || !ids.insert(id) {
                return Err(Error::new(None, Reason::InvalidRecognition));
            }
        }
        let mut bindings = BTreeMap::new();
        for (alias, target) in aliases {
            if alias.is_empty()
                || ids.contains(&alias)
                || !ids.contains(&target)
                || bindings.insert(alias, target).is_some()
            {
                return Err(Error::new(None, Reason::InvalidRecognition));
            }
        }
        Ok(Self {
            canonical: ids,
            aliases: bindings,
        })
    }

    fn canonical<'a>(&'a self, id: &'a str) -> Result<&'a str, Error> {
        if self.canonical.contains(id) {
            Ok(id)
        } else {
            self.aliases
                .get(id)
                .map(String::as_str)
                .ok_or_else(|| Error::new(None, Reason::UnknownModel))
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    recipe_options: Document,
    execution_profiles: Document,
    user_models: Document,
    recognized: RecognizedRows,
}

impl fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Settings { .. }")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authority {
    LegacyRecipeOptions,
    ProfileSettingsV1,
}

pub struct SelectionContext<'a> {
    pub current_recipe: &'a str,
    pub effective_options: &'a SavedOptions,
}

impl fmt::Debug for SelectionContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SelectionContext { .. }")
    }
}

pub struct ProfileSelection(Object);

impl fmt::Debug for ProfileSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProfileSelection { .. }")
    }
}

impl ProfileSelection {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let object = parse_object(bytes, None)?;
        let id = string(&object, "id")
            .filter(|id| !id.is_empty() && id.len() <= 160)
            .ok_or_else(|| Error::new(None, Reason::InvalidSelection))?;
        if id != "registered" && string(&object, "recipe").is_none_or(|recipe| recipe.is_empty()) {
            return Err(Error::new(None, Reason::InvalidSelection));
        }
        if let Some(options) = object.get("options") {
            object_value(options, None)?;
        }
        Ok(Self(object))
    }
}

pub struct PublicProfiles(Object);

impl fmt::Debug for PublicProfiles {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicProfiles { .. }")
    }
}

impl Serialize for PublicProfiles {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl PublicProfiles {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(&self.0, None)
    }
}

impl Settings {
    pub fn from_documents(
        recipe_options: Document,
        execution_profiles: Document,
        user_models: Document,
        recognized: RecognizedRows,
    ) -> Result<Self, Error> {
        if recipe_options.role != Role::RecipeOptions
            || execution_profiles.role != Role::ExecutionProfiles
            || user_models.role != Role::UserModels
        {
            return Err(Error::new(None, Reason::WrongDocumentRole));
        }
        let settings = Self {
            recipe_options,
            execution_profiles,
            user_models,
            recognized,
        };
        settings.validate()?;
        Ok(settings)
    }

    pub fn document(&self, role: Role) -> Option<&Document> {
        match role {
            Role::RecipeOptions => Some(&self.recipe_options),
            Role::ExecutionProfiles => Some(&self.execution_profiles),
            Role::UserModels => Some(&self.user_models),
            _ => None,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        for (id, value) in self.recipe_options.rows.iter() {
            let qualified = ["user.", "extra.", "builtin."]
                .iter()
                .any(|prefix| id.starts_with(prefix));
            if (qualified
                || self.recognized.canonical.contains(id)
                || self.recognized.aliases.contains_key(id))
                && !value.get().trim_start().starts_with('{')
            {
                return Err(Error::new(
                    Some(Role::RecipeOptions),
                    Reason::InvalidRecognizedRow,
                ));
            }
        }
        for id in &self.recognized.canonical {
            self.legacy_key(id)?;
        }
        for value in self.execution_profiles.rows.values() {
            if let Ok(row) = object_value(value, Some(Role::ExecutionProfiles))
                && row.contains_key("settings")
            {
                validate_owned(&row)?;
            }
        }
        Ok(())
    }

    fn legacy_key<'a>(&'a self, id: &'a str) -> Result<Option<&'a str>, Error> {
        let mut found = self.recipe_options.rows.contains_key(id).then_some(id);
        for (alias, target) in &self.recognized.aliases {
            if target == id && self.recipe_options.rows.contains_key(alias) {
                if found.is_some() {
                    return Err(Error::new(
                        Some(Role::RecipeOptions),
                        Reason::ConflictingRows,
                    ));
                }
                found = Some(alias.as_str());
            }
        }
        Ok(found)
    }

    fn profile_row(&self, id: &str) -> Result<Option<Object>, Error> {
        self.execution_profiles
            .rows
            .get(id)
            .map(|row| object_value(row, Some(Role::ExecutionProfiles)))
            .transpose()
    }

    pub fn authority(&self, id: &str) -> Result<Authority, Error> {
        let id = self.recognized.canonical(id)?;
        match self.profile_row(id)? {
            Some(row) if row.contains_key("settings") => {
                validate_owned(&row)?;
                Ok(Authority::ProfileSettingsV1)
            }
            _ => Ok(Authority::LegacyRecipeOptions),
        }
    }

    pub fn saved_options(&self, id: &str) -> Result<SavedOptions, Error> {
        let id = self.recognized.canonical(id)?;
        if self.authority(id)? == Authority::ProfileSettingsV1 {
            let row = self.profile_row(id)?.expect("validated owned row");
            let settings = object_value(&row["settings"], Some(Role::ExecutionProfiles))?;
            return object_value(&settings["saved_options"], Some(Role::ExecutionProfiles))
                .map(SavedOptions);
        }
        match self.legacy_key(id)? {
            Some(key) => object_value(&self.recipe_options.rows[key], Some(Role::RecipeOptions))
                .map(SavedOptions),
            None => Ok(SavedOptions::default()),
        }
    }

    pub fn public_profiles(&self, id: &str) -> Result<PublicProfiles, Error> {
        let id = self.recognized.canonical(id)?;
        let mut row = self.profile_row(id)?.unwrap_or_else(|| {
            BTreeMap::from([
                ("selected".into(), raw(&"registered")),
                ("profiles".into(), raw(&Object::new())),
            ])
        });
        row.remove("settings");
        Ok(PublicProfiles(row))
    }

    pub fn merge_options(&self, id: &str, patch: &SavedOptions) -> Result<Self, Error> {
        let id = self.recognized.canonical(id)?;
        let mut saved = self.saved_options(id)?.0;
        for (key, value) in &patch.0 {
            if value.get() == "null" {
                saved.remove(key);
            } else {
                saved.insert(key.clone(), value.clone());
            }
        }
        let mut next = self.clone();
        if self.authority(id)? == Authority::ProfileSettingsV1 {
            let mut rows = (*self.execution_profiles.rows).clone();
            let mut row = self.profile_row(id)?.expect("validated owned row");
            let mut settings = object_value(&row["settings"], Some(Role::ExecutionProfiles))?;
            settings.insert("saved_options".into(), raw(&saved));
            row.insert("settings".into(), raw(&settings));
            rows.insert(id.into(), raw(&row));
            next.execution_profiles = self.execution_profiles.changed(rows)?;
        } else {
            let mut rows = (*self.recipe_options.rows).clone();
            let key = self.legacy_key(id)?.unwrap_or(id);
            if saved.is_empty() {
                rows.remove(key);
            } else {
                rows.insert(key.into(), raw(&saved));
            }
            next.recipe_options = self.recipe_options.changed(rows)?;
        }
        Ok(next)
    }

    pub fn select_profile(
        &self,
        id: &str,
        context: SelectionContext<'_>,
        selection: &ProfileSelection,
    ) -> Result<Self, Error> {
        let id = self.recognized.canonical(id)?;
        if context.current_recipe.is_empty() {
            return Err(Error::new(None, Reason::InvalidSelection));
        }
        let mut state = self.profile_row(id)?.unwrap_or_default();
        let selected = match state.get("selected") {
            Some(value) => parse_string(value)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| Error::new(None, Reason::InvalidSelection))?,
            None => "registered".into(),
        };
        let mut profiles = optional_object(&state, "profiles")?;
        if let Some(current) = profiles.get(&selected) {
            let current = object_value(current, Some(Role::ExecutionProfiles))?;
            if string(&current, "recipe").is_some_and(|recipe| recipe != context.current_recipe) {
                return Err(Error::new(None, Reason::RecipeChanged));
            }
        }
        let previous = self.saved_options(id)?;
        let mut current = profiles
            .get(&selected)
            .map(|value| object_value(value, Some(Role::ExecutionProfiles)))
            .transpose()?
            .unwrap_or_default();
        current.insert("id".into(), raw(&selected));
        current.insert("recipe".into(), raw(&context.current_recipe));
        let mut current_options = optional_object(&current, "options")?;
        current_options.extend(context.effective_options.0.clone());
        current.insert("options".into(), raw(&current_options));
        if selected == "registered" {
            current.insert("saved_options".into(), raw(&previous));
        }
        profiles.insert(selected, raw(&current));
        let requested = string(&selection.0, "id").expect("validated selection");
        if requested != "registered" {
            let mut target = profiles
                .get(&requested)
                .map(|value| object_value(value, Some(Role::ExecutionProfiles)))
                .transpose()?
                .unwrap_or_default();
            target.extend(selection.0.clone());
            profiles.insert(requested.clone(), raw(&target));
        }
        let mut target = profiles
            .get(&requested)
            .ok_or_else(|| Error::new(None, Reason::InvalidSelection))
            .and_then(|value| object_value(value, Some(Role::ExecutionProfiles)))?;
        if string(&target, "recipe").is_none_or(|recipe| recipe.is_empty()) {
            return Err(Error::new(None, Reason::InvalidSelection));
        }
        let pinned = match context.effective_options.get("pinned") {
            Some(value) => serde_json::from_str::<bool>(value.get())
                .map_err(|_| Error::new(None, Reason::InvalidSelection))?,
            None => false,
        };
        let mut options = optional_object(&target, "options")?;
        options.insert("pinned".into(), raw(&pinned));
        target.insert("options".into(), raw(&options));
        let mut saved = if requested == "registered" {
            target
                .get("saved_options")
                .map(|value| object_value(value, Some(Role::ExecutionProfiles)))
                .transpose()?
                .unwrap_or(options)
        } else {
            options
        };
        saved.insert("pinned".into(), raw(&pinned));
        profiles.insert(requested.clone(), raw(&target));
        let mut owned = optional_object(&state, "settings")?;
        owned.insert("version".into(), raw(&1));
        owned.insert("saved_options".into(), raw(&saved));
        state.insert("selected".into(), raw(&requested));
        state.insert("profiles".into(), raw(&profiles));
        state.insert("settings".into(), raw(&owned));
        validate_owned(&state)?;
        let mut rows = (*self.execution_profiles.rows).clone();
        rows.insert(id.into(), raw(&state));
        let mut next = self.clone();
        next.execution_profiles = self.execution_profiles.changed(rows)?;
        Ok(next)
    }
}

fn validate_owned(row: &Object) -> Result<(), Error> {
    let invalid = || Error::new(Some(Role::ExecutionProfiles), Reason::InvalidOwnedSettings);
    let settings = row.get("settings").ok_or_else(invalid).and_then(|value| {
        object_value(value, Some(Role::ExecutionProfiles)).map_err(|_| invalid())
    })?;
    if settings
        .get("version")
        .and_then(|value| serde_json::from_str::<u64>(value.get()).ok())
        != Some(1)
    {
        return Err(invalid());
    }
    if settings
        .get("saved_options")
        .is_none_or(|value| object_value(value, Some(Role::ExecutionProfiles)).is_err())
    {
        return Err(invalid());
    }
    let selected = string(row, "selected")
        .filter(|id| !id.is_empty())
        .ok_or_else(invalid)?;
    let profiles = row.get("profiles").ok_or_else(invalid).and_then(|value| {
        object_value(value, Some(Role::ExecutionProfiles)).map_err(|_| invalid())
    })?;
    let profile = profiles
        .get(&selected)
        .ok_or_else(invalid)
        .and_then(|value| {
            object_value(value, Some(Role::ExecutionProfiles)).map_err(|_| invalid())
        })?;
    if string(&profile, "recipe").is_none_or(|recipe| recipe.is_empty()) {
        return Err(invalid());
    }
    Ok(())
}

fn optional_object(object: &Object, key: &str) -> Result<Object, Error> {
    object
        .get(key)
        .map(|value| object_value(value, Some(Role::ExecutionProfiles)))
        .transpose()
        .map(Option::unwrap_or_default)
}
fn parse_string(value: &RawValue) -> Option<String> {
    serde_json::from_str(value.get()).ok()
}
fn string(object: &Object, key: &str) -> Option<String> {
    object.get(key).and_then(|value| parse_string(value))
}
fn object_value(value: &RawValue, role: Option<Role>) -> Result<Object, Error> {
    serde_json::from_str(value.get()).map_err(|_| Error::new(role, Reason::NotObject))
}
fn raw(value: &impl Serialize) -> Arc<RawValue> {
    serde_json::value::to_raw_value(value)
        .expect("native settings values contain only valid JSON")
        .into()
}
fn parse_object(bytes: &[u8], role: Option<Role>) -> Result<Object, Error> {
    scan(bytes, role)?;
    let value: &RawValue =
        serde_json::from_slice(bytes).map_err(|_| Error::new(role, Reason::InvalidJson))?;
    object_value(value, role)
}

struct ExpectedBytes<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Write for ExpectedBytes<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = self
            .at
            .checked_add(bytes.len())
            .filter(|end| *end <= MAX_DOCUMENT_BYTES)
            .ok_or_else(|| std::io::Error::other("native settings view mismatch"))?;
        if self.bytes.get(self.at..end) != Some(bytes) {
            return Err(std::io::Error::other("native settings view mismatch"));
        }
        self.at = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct BoundedBuffer(Vec<u8>);
impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_DOCUMENT_BYTES {
            return Err(std::io::Error::other("native settings size bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(object: &Object, role: Option<Role>) -> Result<Vec<u8>, Error> {
    let mut output = BoundedBuffer(Vec::new());
    serde_json::to_writer(&mut output, object)
        .map_err(|_| Error::new(role, Reason::FileTooLarge))?;
    Ok(output.0)
}

fn scan(bytes: &[u8], role: Option<Role>) -> Result<(), Error> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::new(role, Reason::FileTooLarge));
    }
    let mut scan = Scan {
        bytes,
        at: 0,
        events: 0,
        role,
    };
    scan.value(0)?;
    scan.whitespace();
    if scan.at != bytes.len() {
        return Err(Error::new(role, Reason::InvalidJson));
    }
    Ok(())
}
struct Scan<'a> {
    bytes: &'a [u8],
    at: usize,
    events: usize,
    role: Option<Role>,
}
impl Scan<'_> {
    fn error(&self, reason: Reason) -> Error {
        Error::new(self.role, reason)
    }
    fn event(&mut self, depth: usize) -> Result<(), Error> {
        if depth > MAX_DEPTH {
            return Err(self.error(Reason::DepthLimit));
        }
        self.events += 1;
        if self.events > MAX_EVENTS {
            return Err(self.error(Reason::EventLimit));
        }
        Ok(())
    }
    fn whitespace(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }
    fn consume(&mut self, byte: u8) -> bool {
        self.whitespace();
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn text(&mut self) -> Result<&[u8], Error> {
        self.whitespace();
        let start = self.at;
        if self.bytes.get(self.at) != Some(&b'"') {
            return Err(self.error(Reason::InvalidJson));
        }
        self.at += 1;
        while let Some(byte) = self.bytes.get(self.at) {
            self.at += 1;
            match byte {
                b'"' => return Ok(&self.bytes[start..self.at]),
                b'\\' => self.at += 1,
                _ => {}
            }
        }
        Err(self.error(Reason::InvalidJson))
    }
    fn value(&mut self, depth: usize) -> Result<(), Error> {
        self.whitespace();
        self.event(depth)?;
        match self.bytes.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut keys = BTreeSet::new();
                if !self.consume(b'}') {
                    loop {
                        self.event(depth + 1)?;
                        let role = self.role;
                        let key: String = serde_json::from_slice(self.text()?)
                            .map_err(|_| Error::new(role, Reason::InvalidJson))?;
                        if !keys.insert(key) {
                            return Err(self.error(Reason::DuplicateKey));
                        }
                        if !self.consume(b':') {
                            return Err(self.error(Reason::InvalidJson));
                        }
                        self.value(depth + 1)?;
                        if self.consume(b'}') {
                            break;
                        }
                        if !self.consume(b',') {
                            return Err(self.error(Reason::InvalidJson));
                        }
                    }
                }
                self.event(depth)
            }
            Some(b'[') => {
                self.at += 1;
                if !self.consume(b']') {
                    loop {
                        self.value(depth + 1)?;
                        if self.consume(b']') {
                            break;
                        }
                        if !self.consume(b',') {
                            return Err(self.error(Reason::InvalidJson));
                        }
                    }
                }
                self.event(depth)
            }
            Some(b'"') => {
                self.text()?;
                Ok(())
            }
            Some(_) => {
                let start = self.at;
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && !b",]}".contains(byte))
                {
                    self.at += 1;
                }
                if start == self.at {
                    Err(self.error(Reason::InvalidJson))
                } else {
                    Ok(())
                }
            }
            None => Err(self.error(Reason::InvalidJson)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn unregistered_qualified_rows_are_settings_but_unknown_bare_metadata_stays_opaque() {
        let known =
            || RecognizedRows::new(Vec::<String>::new(), Vec::<(String, String)>::new()).unwrap();
        for id in [
            "user.Stale",
            "extra.Stale",
            "builtin.Stale",
            "user.",
            "extra.",
            "builtin.",
        ] {
            let bytes = format!("{{{}:null}}", serde_json::to_string(id).unwrap());
            let failure = Settings::from_documents(
                Document::parse_bytes(Role::RecipeOptions, Some(bytes.as_bytes())).unwrap(),
                Document::parse_bytes(Role::ExecutionProfiles, None).unwrap(),
                Document::parse_bytes(Role::UserModels, None).unwrap(),
                known(),
            )
            .err()
            .unwrap();
            assert_eq!(failure.reason, Reason::InvalidRecognizedRow);
        }
        let bytes = br#"{"unknown bare metadata":[null,42],"builtin.Stale":{"future":-1}}"#;
        let state = Settings::from_documents(
            Document::parse_bytes(Role::RecipeOptions, Some(bytes)).unwrap(),
            Document::parse_bytes(Role::ExecutionProfiles, None).unwrap(),
            Document::parse_bytes(Role::UserModels, None).unwrap(),
            known(),
        )
        .unwrap();
        assert_eq!(
            state
                .document(Role::RecipeOptions)
                .unwrap()
                .to_bytes()
                .unwrap(),
            bytes
        );
        assert_eq!(
            state.saved_options("builtin.Stale").err().unwrap().reason,
            Reason::UnknownModel
        );
    }

    const MODEL: &str = "user.Atomic-Settings";

    fn recognized() -> RecognizedRows {
        RecognizedRows::new(
            [
                MODEL.into(),
                "builtin.Known".into(),
                "extra.Unregistered-Future".into(),
            ],
            [("Known".into(), "builtin.Known".into())],
        )
        .unwrap()
    }
    fn settings(recipe: Option<&[u8]>, profiles: Option<&[u8]>) -> Result<Settings, Error> {
        Settings::from_documents(
            Document::parse_bytes(Role::RecipeOptions, recipe)?,
            Document::parse_bytes(Role::ExecutionProfiles, profiles)?,
            Document::parse_bytes(Role::UserModels, None)?,
            recognized(),
        )
    }
    fn options(json: &str) -> SavedOptions {
        SavedOptions::parse(json.as_bytes()).unwrap()
    }
    fn selection(json: &str) -> ProfileSelection {
        ProfileSelection::parse(json.as_bytes()).unwrap()
    }
    fn selected(settings: &Settings) -> String {
        let row = settings.profile_row(MODEL).unwrap().unwrap();
        string(&row, "selected").unwrap()
    }
    fn context(settings: &Settings) -> i64 {
        serde_json::from_str(
            settings
                .saved_options(MODEL)
                .unwrap()
                .get("ctx_size")
                .unwrap()
                .get(),
        )
        .unwrap()
    }

    #[test]
    fn missing_is_distinct_from_present_damaged_documents_and_originals_are_shared() {
        for role in [
            Role::Config,
            Role::Jobs,
            Role::NativeHelpers,
            Role::Aliases,
            Role::RecipeOptions,
            Role::ExecutionProfiles,
            Role::UserModels,
        ] {
            let absent = Document::parse(role, None).unwrap();
            assert!(absent.is_absent());
            assert!(absent.original_bytes().is_none());
            for bytes in [
                b"".as_slice(),
                b"{incomplete",
                b"null",
                b"[]",
                b"[1]",
                b"7",
                b"true",
                b"\"invalid metadata\"",
            ] {
                let error = Document::parse_bytes(role, Some(bytes)).unwrap_err();
                assert_eq!(error.code(), "model_settings_recovery_required");
                assert_eq!(error.role, Some(role));
            }
            let original: Arc<Vec<u8>> =
                Arc::new(b" { \"opaque\" : 123456789012345678901234567890 } \n".to_vec());
            let document = Document::parse(role, Some(original.clone())).unwrap();
            assert!(!document.is_absent());
            assert!(Arc::ptr_eq(document.original_bytes().unwrap(), &original));
            assert_eq!(document.to_bytes().unwrap().as_slice(), original.as_ref());
        }
    }

    #[test]
    fn near_limit_untouched_rows_share_raw_nodes_across_option_edits_and_profile_selection() {
        let large = "x".repeat(MAX_DOCUMENT_BYTES - 4096);
        let recipe_bytes = Arc::new(format!(r#"{{"opaque":{{"large":"{large}","number":123456789012345678901234567890}},"user.Atomic-Settings":{{"ctx_size":2048}}}}"#).into_bytes());
        let profile_bytes = Arc::new(format!(r#"{{"unregistered-opaque":{{"large":"{large}","number":1.23450000000000000000000000e+9999}}}}"#).into_bytes());
        drop(large);
        let state = Settings::from_documents(
            Document::parse(Role::RecipeOptions, Some(recipe_bytes.clone())).unwrap(),
            Document::parse(Role::ExecutionProfiles, Some(profile_bytes.clone())).unwrap(),
            Document::parse(Role::UserModels, None).unwrap(),
            recognized(),
        )
        .unwrap();
        let edited = state
            .merge_options(MODEL, &options(r#"{"ctx_size":8192}"#))
            .unwrap();
        assert!(std::ptr::eq(
            state.recipe_options.row("opaque").unwrap(),
            edited.recipe_options.row("opaque").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &state.recipe_options.rows["opaque"],
            &edited.recipe_options.rows["opaque"]
        ));
        assert!(!std::ptr::eq(
            state.recipe_options.row(MODEL).unwrap(),
            edited.recipe_options.row(MODEL).unwrap()
        ));
        assert_eq!(context(&state), 2048);
        assert_eq!(context(&edited), 8192);
        let adopted = edited
            .select_profile(
                MODEL,
                SelectionContext {
                    current_recipe: "llamacpp",
                    effective_options: &options(r#"{"ctx_size":8192}"#),
                },
                &selection(r#"{"id":"registered"}"#),
            )
            .unwrap();
        assert!(Arc::ptr_eq(
            &state.execution_profiles.rows["unregistered-opaque"],
            &adopted.execution_profiles.rows["unregistered-opaque"]
        ));
        assert!(std::ptr::eq(
            state.execution_profiles.row("unregistered-opaque").unwrap(),
            adopted
                .execution_profiles
                .row("unregistered-opaque")
                .unwrap()
        ));
        assert!(Arc::ptr_eq(
            state.recipe_options.original_bytes().unwrap(),
            adopted.recipe_options.original_bytes().unwrap()
        ));
        assert!(Arc::ptr_eq(
            state.execution_profiles.original_bytes().unwrap(),
            adopted.execution_profiles.original_bytes().unwrap()
        ));
        assert_eq!(
            adopted.recipe_options.original_bytes().unwrap().as_ref(),
            recipe_bytes.as_ref()
        );
        assert_eq!(
            adopted
                .execution_profiles
                .original_bytes()
                .unwrap()
                .as_ref(),
            profile_bytes.as_ref()
        );
        assert_eq!(state.recipe_options.to_bytes().unwrap(), *recipe_bytes);
        assert_eq!(state.execution_profiles.to_bytes().unwrap(), *profile_bytes);
        assert!(
            adopted
                .execution_profiles
                .row("unregistered-opaque")
                .unwrap()
                .get()
                .contains("1.23450000000000000000000000e+9999")
        );
        assert_eq!(
            adopted.authority(MODEL).unwrap(),
            Authority::ProfileSettingsV1
        );
        assert_eq!(
            state.authority(MODEL).unwrap(),
            Authority::LegacyRecipeOptions
        );
    }

    #[test]
    fn rebasing_unchanged_documents_adopts_exact_buffers_without_reparsing_or_changing_rows() {
        let bytes = Arc::new(b" { \"opaque\" : 123456789012345678901234567890 } \n".to_vec());
        let document = Document::parse(Role::RecipeOptions, Some(bytes.clone())).unwrap();
        let same = document.rebase_current(Some(bytes.clone())).unwrap();
        assert!(Arc::ptr_eq(same.original_bytes().unwrap(), &bytes));
        assert!(document.same_view(&same));
        let copied = Arc::new((*bytes).clone());
        let adopted = document.rebase_current(Some(copied.clone())).unwrap();
        assert!(Arc::ptr_eq(adopted.original_bytes().unwrap(), &copied));
        assert!(!Arc::ptr_eq(adopted.original_bytes().unwrap(), &bytes));
        assert!(document.same_view(&adopted));
        assert!(!adopted.dirty);
        assert_eq!(adopted.to_bytes().unwrap(), *bytes);
        let absent = Document::parse(Role::ExecutionProfiles, None).unwrap();
        assert!(absent.same_view(&absent.rebase_current(None).unwrap()));
        assert_eq!(
            absent
                .rebase_current(Some(Arc::new(b"{}".to_vec())))
                .unwrap_err()
                .reason,
            Reason::CurrentViewMismatch
        );
        assert_eq!(
            document.rebase_current(None).unwrap_err().reason,
            Reason::CurrentViewMismatch
        );
    }

    #[test]
    fn dirty_rebase_compares_streamed_current_bytes_exactly_and_retains_large_shared_rows() {
        let source = format!(
            r#"{{"user.Atomic-Settings":{{"ctx_size":2048}},"opaque":{{"large":"{}","n":1.230000000000000000000e+9999}}}}"#,
            "x".repeat(1024 * 1024)
        );
        let state = settings(Some(source.as_bytes()), None).unwrap();
        let changed = state
            .merge_options(MODEL, &options(r#"{"ctx_size":4096}"#))
            .unwrap();
        let document = changed.document(Role::RecipeOptions).unwrap();
        assert!(document.dirty);
        let current = Arc::new(document.to_bytes().unwrap());
        let rebased = document.rebase_current(Some(current.clone())).unwrap();
        assert!(!rebased.dirty);
        assert!(rebased.same_view(document));
        assert!(Arc::ptr_eq(rebased.original_bytes().unwrap(), &current));
        assert!(Arc::ptr_eq(
            &rebased.rows["opaque"],
            &state.recipe_options.rows["opaque"]
        ));
        assert_eq!(state.recipe_options.to_bytes().unwrap(), source.as_bytes());
        let wrong = state.recipe_options.original_bytes().unwrap().clone();
        assert_eq!(
            document.rebase_current(Some(wrong)).unwrap_err().reason,
            Reason::CurrentViewMismatch
        );
        let mut trailing = (*current).clone();
        trailing.push(b' ');
        assert_eq!(
            document
                .rebase_current(Some(Arc::new(trailing)))
                .unwrap_err()
                .reason,
            Reason::CurrentViewMismatch
        );
        let mut truncated = (*current).clone();
        truncated.pop();
        assert_eq!(
            document
                .rebase_current(Some(Arc::new(truncated)))
                .unwrap_err()
                .reason,
            Reason::CurrentViewMismatch
        );
        let oversized = Arc::new(vec![b' '; MAX_DOCUMENT_BYTES + 1]);
        assert_eq!(
            document.rebase_current(Some(oversized)).unwrap_err().reason,
            Reason::FileTooLarge
        );
        let marker = "JUICED_SYNTHETIC_PRIVATE_REBASE_MARKER";
        let error = document
            .rebase_current(Some(Arc::new(
                format!(r#"{{"{marker}":"{marker}"}}"#).into_bytes(),
            )))
            .unwrap_err();
        assert!(!format!("{error} {error:?}").contains(marker));
        assert!(error.source().is_none());
    }

    #[test]
    fn qualified_and_explicit_rows_validate_while_unknown_bare_metadata_stays_opaque() {
        for id in [MODEL, "Known", "builtin.Known", "extra.Unregistered-Future"] {
            for invalid in ["null", "[1]", "7", "true", "\"invalid options\""] {
                let bytes = format!("{{{}:{invalid}}}", serde_json::to_string(id).unwrap());
                assert_eq!(
                    settings(Some(bytes.as_bytes()), None).unwrap_err().reason,
                    Reason::InvalidRecognizedRow
                );
            }
        }
        let recipe = br#"{"future-nonmodel-metadata":[1,"retained"],"Unknown-Builtin":7,"builtin.Not-Supplied":{"opaque":false},"Known":{"ctx_size":2048}}"#;
        let state = settings(Some(recipe), None).unwrap();
        assert_eq!(
            state
                .saved_options("builtin.Known")
                .unwrap()
                .get("ctx_size")
                .unwrap()
                .get(),
            "2048"
        );
        assert_eq!(
            state
                .saved_options("Known")
                .unwrap()
                .get("ctx_size")
                .unwrap()
                .get(),
            "2048"
        );
        assert_eq!(
            state.saved_options("Unknown-Builtin").unwrap_err().reason,
            Reason::UnknownModel
        );
        let next = state
            .merge_options("Known", &options(r#"{"ctx_size":4096}"#))
            .unwrap();
        for key in [
            "future-nonmodel-metadata",
            "Unknown-Builtin",
            "builtin.Not-Supplied",
        ] {
            assert_eq!(
                next.recipe_options.row(key).unwrap().get(),
                state.recipe_options.row(key).unwrap().get()
            );
        }
        assert!(next.recipe_options.row("builtin.Known").is_none());
        let registrations = Document::parse_bytes(
            Role::UserModels,
            Some(br#"{"uninterpreted":null,"legacy":[1,2]}"#),
        )
        .unwrap();
        Settings::from_documents(
            state.recipe_options,
            state.execution_profiles,
            registrations,
            recognized(),
        )
        .unwrap();
    }

    #[test]
    fn clearing_legacy_rows_and_default_profile_projection_do_not_create_a_second_authority() {
        let source = br#"{"user.Atomic-Settings":{"ctx_size":2048},"opaque":[1,2]}"#;
        let state = settings(Some(source), None).unwrap();
        let next = state
            .merge_options(MODEL, &options(r#"{"ctx_size":null,"absent":null}"#))
            .unwrap();
        assert!(next.recipe_options.row(MODEL).is_none());
        assert_eq!(next.recipe_options.row("opaque").unwrap().get(), "[1,2]");
        assert!(next.saved_options(MODEL).unwrap().0.is_empty());
        assert_eq!(
            next.authority(MODEL).unwrap(),
            Authority::LegacyRecipeOptions
        );
        assert!(next.execution_profiles.is_absent());
        assert_eq!(
            next.public_profiles(MODEL).unwrap().to_bytes().unwrap(),
            br#"{"profiles":{},"selected":"registered"}"#
        );
        assert_eq!(
            next.recipe_options.original_bytes().unwrap().as_ref(),
            source
        );
        assert_eq!(state.recipe_options.to_bytes().unwrap(), source);
        assert_eq!(Role::NativeHelpers.filename(), "native_helpers.json");
        assert!(state.document(Role::NativeHelpers).is_none());
        let error = Settings::from_documents(
            Document::parse(Role::Config, None).unwrap(),
            state.execution_profiles.clone(),
            state.user_models.clone(),
            recognized(),
        )
        .unwrap_err();
        assert_eq!(error.reason, Reason::WrongDocumentRole);
    }

    #[test]
    fn recognition_collisions_and_multiple_saved_rows_reject_without_guessing() {
        assert_eq!(
            RecognizedRows::new(["id".into(), "id".into()], [])
                .unwrap_err()
                .reason,
            Reason::InvalidRecognition
        );
        assert!(RecognizedRows::new(["id".into()], [("id".into(), "id".into())]).is_err());
        assert!(RecognizedRows::new(["id".into()], [("alias".into(), "absent".into())]).is_err());
        assert!(
            RecognizedRows::new(
                ["id".into()],
                [("alias".into(), "id".into()), ("alias".into(), "id".into())]
            )
            .is_err()
        );
        assert_eq!(
            settings(Some(br#"{"Known":{},"builtin.Known":{}}"#), None)
                .unwrap_err()
                .reason,
            Reason::ConflictingRows
        );
    }

    #[test]
    fn legacy_authority_fallback_unknowns_and_null_clears_preserve_explicit_negative_values() {
        let recipe = br#" {"user.Atomic-Settings":{"ctx_size":4096,"legacy_unknown":"untouched"},"unknown":[1,2]} "#;
        let profiles = br#"{"user.Atomic-Settings":{"selected":"alternate","profiles":{"registered":{"id":"registered","recipe":"llamacpp","options":{"ctx_size":1536},"saved_options":{"ctx_size":2048,"future_option":"preserved"}},"alternate":{"id":"alternate","recipe":"vllm","options":{"ctx_size":4096}}}},"future-row":{"opaque":1}}"#;
        let state = settings(Some(recipe), Some(profiles)).unwrap();
        assert_eq!(
            state.authority(MODEL).unwrap(),
            Authority::LegacyRecipeOptions
        );
        assert_eq!(context(&state), 4096);
        let adopted = state
            .select_profile(
                MODEL,
                SelectionContext {
                    current_recipe: "vllm",
                    effective_options: &options(r#"{"ctx_size":4096}"#),
                },
                &selection(r#"{"id":"registered"}"#),
            )
            .unwrap();
        assert_eq!(
            adopted.authority(MODEL).unwrap(),
            Authority::ProfileSettingsV1
        );
        assert_eq!(selected(&adopted), "registered");
        assert_eq!(context(&adopted), 2048);
        assert_eq!(
            adopted
                .saved_options(MODEL)
                .unwrap()
                .get("future_option")
                .unwrap()
                .get(),
            r#""preserved""#
        );
        assert_eq!(adopted.recipe_options.to_bytes().unwrap(), recipe);
        let cleared = adopted
            .merge_options(MODEL, &options(r#"{"ctx_size":null}"#))
            .unwrap();
        assert!(
            cleared
                .saved_options(MODEL)
                .unwrap()
                .get("ctx_size")
                .is_none()
        );
        let row = cleared.profile_row(MODEL).unwrap().unwrap();
        let definitions = optional_object(&row, "profiles").unwrap();
        let registered = object_value(&definitions["registered"], None).unwrap();
        assert_eq!(
            optional_object(&registered, "options").unwrap()["ctx_size"].get(),
            "1536"
        );
        let negative = cleared
            .merge_options(MODEL, &options(r#"{"ctx_size":-1}"#))
            .unwrap();
        assert_eq!(context(&negative), -1);
        let restarted = Settings::from_documents(
            negative.recipe_options.clone(),
            Document::parse_bytes(
                Role::ExecutionProfiles,
                Some(&negative.execution_profiles.to_bytes().unwrap()),
            )
            .unwrap(),
            negative.user_models.clone(),
            recognized(),
        )
        .unwrap();
        assert_eq!(context(&restarted), -1);
        assert_eq!(restarted.recipe_options.to_bytes().unwrap(), recipe);
        assert_eq!(
            negative
                .execution_profiles
                .original_bytes()
                .unwrap()
                .as_ref(),
            profiles
        );
        assert_eq!(context(&state), 4096);
    }

    #[test]
    fn first_selection_atomically_adopts_saved_options_and_restores_registered_selection() {
        let recipe = br#"{"user.Atomic-Settings":{"ctx_size":2048,"future_option":"preserved"}}"#;
        let state = settings(Some(recipe), None).unwrap();
        let alternate = state.select_profile(MODEL, SelectionContext { current_recipe: "llamacpp", effective_options: &options(r#"{"ctx_size":2048,"pinned":true}"#) }, &selection(r#"{"id":"alternate","recipe":"vllm","options":{"ctx_size":4096},"unknown_definition":{"large":123456789012345678901234567890}}"#)).unwrap();
        assert_eq!(selected(&alternate), "alternate");
        assert_eq!(context(&alternate), 4096);
        assert_eq!(
            alternate
                .saved_options(MODEL)
                .unwrap()
                .get("pinned")
                .unwrap()
                .get(),
            "true"
        );
        assert_eq!(alternate.recipe_options.to_bytes().unwrap(), recipe);
        let public = alternate.public_profiles(MODEL).unwrap();
        let bytes = public.to_bytes().unwrap();
        assert!(
            !object_value(
                &serde_json::value::RawValue::from_string(String::from_utf8(bytes).unwrap())
                    .unwrap(),
                None
            )
            .unwrap()
            .contains_key("settings")
        );
        let restored = alternate
            .select_profile(
                MODEL,
                SelectionContext {
                    current_recipe: "vllm",
                    effective_options: &options(r#"{"ctx_size":4096,"pinned":true}"#),
                },
                &selection(r#"{"id":"registered"}"#),
            )
            .unwrap();
        assert_eq!(context(&restored), 2048);
        assert_eq!(
            restored
                .saved_options(MODEL)
                .unwrap()
                .get("future_option")
                .unwrap()
                .get(),
            r#""preserved""#
        );
        let profiles =
            optional_object(&restored.profile_row(MODEL).unwrap().unwrap(), "profiles").unwrap();
        assert!(
            profiles["alternate"]
                .get()
                .contains("123456789012345678901234567890")
        );
        assert_eq!(
            state.authority(MODEL).unwrap(),
            Authority::LegacyRecipeOptions
        );
    }

    #[test]
    fn unknown_owned_fields_stale_rows_and_number_lexemes_survive_edits_and_projection() {
        let profiles = br#"{"user.Atomic-Settings":{"selected":"registered","profiles":{"registered":{"recipe":"llamacpp","options":{"ctx_size":2048,"unrecognized":1e9999},"definition_unknown":[0]},"unused":{"recipe":"future-engine","nested":{"n":999999999999999999999999999999999999}}},"settings":{"version":1,"saved_options":{"ctx_size":2048,"opaque":{"a":1.23000000000000000000000000e+1234}},"future_internal":1e-9999},"row_unknown":-0.0000},"builtin.Stale":{"selected":"future","profiles":{"future":{"recipe":"future-engine"}},"settings":{"version":1,"saved_options":{"future":1e9000}}},"unknown-row":[1,2]}"#;
        let state = settings(None, Some(profiles)).unwrap();
        let next = state
            .merge_options(
                MODEL,
                &options(r#"{"ctx_size":8192,"new_unknown":{"n":123456789012345678901234567890}}"#),
            )
            .unwrap();
        assert_eq!(
            next.saved_options(MODEL)
                .unwrap()
                .get("opaque")
                .unwrap()
                .get(),
            r#"{"a":1.23000000000000000000000000e+1234}"#
        );
        assert_eq!(
            state.execution_profiles.row("builtin.Stale").unwrap().get(),
            next.execution_profiles.row("builtin.Stale").unwrap().get()
        );
        let bytes = next.execution_profiles.to_bytes().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        for lexeme in [
            "1e9999",
            "1e-9999",
            "-0.0000",
            "999999999999999999999999999999999999",
            "123456789012345678901234567890",
        ] {
            assert!(text.contains(lexeme));
        }
        let public = next.public_profiles(MODEL).unwrap().to_bytes().unwrap();
        let public = std::str::from_utf8(&public).unwrap();
        assert!(!public.contains("future_internal"));
        assert!(!public.contains("saved_options"));
        assert!(public.contains("row_unknown"));
        assert_eq!(
            next.execution_profiles.original_bytes().unwrap().as_ref(),
            profiles
        );
    }

    #[test]
    fn incomplete_and_unsupported_owned_rows_reject_without_legacy_fallback() {
        let base = serde_json::json!({"selected":"registered","profiles":{"registered":{"recipe":"llamacpp"}},"settings":{"version":1,"saved_options":{}}});
        for variant in 0..11 {
            let mut row = base.clone();
            match variant {
                0 => row["settings"]["version"] = serde_json::json!(2),
                1 => row["settings"]["saved_options"] = serde_json::json!([]),
                2 => {
                    row["profiles"]["registered"]
                        .as_object_mut()
                        .unwrap()
                        .remove("recipe");
                }
                3 => row["selected"] = serde_json::json!("missing"),
                4 => row["settings"]["version"] = serde_json::json!("1"),
                5 => row["settings"]["version"] = serde_json::json!(1.0),
                6 => row["settings"] = serde_json::Value::Null,
                7 => row["selected"] = serde_json::json!(""),
                8 => row["profiles"]["registered"]["recipe"] = serde_json::json!(""),
                9 => row["profiles"] = serde_json::json!([]),
                _ => {
                    row.as_object_mut().unwrap().remove("selected");
                }
            }
            let profiles = serde_json::to_vec(&serde_json::json!({MODEL:row})).unwrap();
            assert_eq!(
                settings(
                    Some(br#"{"user.Atomic-Settings":{"ctx_size":2048}}"#),
                    Some(&profiles)
                )
                .unwrap_err()
                .reason,
                Reason::InvalidOwnedSettings
            );
        }
        assert!(
            settings(
                None,
                Some(br#"{"unregistered":{"settings":{"version":2}}}"#)
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_keys_encodings_shapes_and_grammar_are_rejected_without_private_diagnostics() {
        let marker = "JUICED_SYNTHETIC_NATIVE_PRIVATE_MARKER";
        for bytes in [
            format!(r#"{{"{marker}":1,"{marker}":2}}"#).into_bytes(),
            format!(r#"{{"opaque":{{"{marker}":1,"{marker}":2}}}}"#).into_bytes(),
            br#"{"a":1,"\u0061":2}"#.to_vec(),
            vec![b'{', b'"', 0xff, b'"', b':', b'0', b'}'],
            format!(r#""{marker}""#).into_bytes(),
            format!(r#"{{"{marker}":01}}"#).into_bytes(),
            format!(r#"{{"{marker}":true false}}"#).into_bytes(),
            format!(r#"{{"{marker}":1,}}"#).into_bytes(),
        ] {
            let original = bytes.clone();
            let error = Document::parse_bytes(Role::RecipeOptions, Some(&bytes)).unwrap_err();
            assert!(!format!("{error}").contains(marker));
            assert!(!format!("{error:?}").contains(marker));
            assert!(error.source().is_none());
            assert_eq!(bytes, original);
        }
        let semantic = settings(
            None,
            Some(format!(r#"{{"{marker}":{{"settings":{{"version":"{marker}"}}}}}}"#).as_bytes()),
        )
        .unwrap_err();
        assert!(!format!("{semantic:?} {semantic}").contains(marker));
    }

    #[test]
    fn native_depth_event_and_per_file_boundaries_are_enforced_before_and_after_edits() {
        let nested = |count: usize| {
            format!(
                r#"{{"opaque":{}{} }}"#,
                "[".repeat(count),
                "]".repeat(count)
            )
        };
        Document::parse_bytes(Role::UserModels, Some(nested(MAX_DEPTH).as_bytes())).unwrap();
        assert_eq!(
            Document::parse_bytes(Role::UserModels, Some(nested(MAX_DEPTH + 1).as_bytes()))
                .unwrap_err()
                .reason,
            Reason::DepthLimit
        );
        let events = |count: usize| {
            format!(
                r#"{{"opaque":[{}]}}"#,
                std::iter::repeat_n("0", count)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        Document::parse_bytes(Role::UserModels, Some(events(MAX_EVENTS - 5).as_bytes())).unwrap();
        assert_eq!(
            Document::parse_bytes(Role::UserModels, Some(events(MAX_EVENTS - 4).as_bytes()))
                .unwrap_err()
                .reason,
            Reason::EventLimit
        );
        let mut exact = vec![b' '; MAX_DOCUMENT_BYTES];
        exact[..2].copy_from_slice(b"{}");
        let document = Document::parse_bytes(Role::UserModels, Some(&exact)).unwrap();
        assert_eq!(document.to_bytes().unwrap(), exact);
        exact.push(b' ');
        assert_eq!(
            Document::parse_bytes(Role::UserModels, Some(&exact))
                .unwrap_err()
                .reason,
            Reason::FileTooLarge
        );
        let state = settings(None, None).unwrap();
        let patch = SavedOptions::parse(nested(MAX_DEPTH).as_bytes()).unwrap();
        assert_eq!(
            state.merge_options(MODEL, &patch).unwrap_err().reason,
            Reason::DepthLimit
        );
        assert!(state.recipe_options.is_absent());
    }

    #[test]
    fn rejected_selections_and_candidates_leave_the_previous_authority_unchanged() {
        let state = settings(Some(br#"{"user.Atomic-Settings":{"ctx_size":2048}}"#), None).unwrap();
        for bytes in [
            br#"{}"#.as_slice(),
            br#"{"id":""}"#,
            br#"{"id":"other"}"#,
            br#"{"id":"other","recipe":"llamacpp","options":[]}"#,
        ] {
            assert!(ProfileSelection::parse(bytes).is_err());
        }
        let adopted = state
            .select_profile(
                MODEL,
                SelectionContext {
                    current_recipe: "llamacpp",
                    effective_options: &options(r#"{"ctx_size":2048}"#),
                },
                &selection(r#"{"id":"registered"}"#),
            )
            .unwrap();
        assert_eq!(
            adopted
                .select_profile(
                    MODEL,
                    SelectionContext {
                        current_recipe: "wrong",
                        effective_options: &options("{}")
                    },
                    &selection(r#"{"id":"registered"}"#)
                )
                .unwrap_err()
                .reason,
            Reason::RecipeChanged
        );
        assert_eq!(context(&adopted), 2048);
        let giant = format!(r#"{{"private":"{}"}}"#, "x".repeat(MAX_DOCUMENT_BYTES - 32));
        let patch = SavedOptions::parse(giant.as_bytes()).unwrap();
        assert_eq!(
            adopted.merge_options(MODEL, &patch).unwrap_err().reason,
            Reason::FileTooLarge
        );
        assert_eq!(context(&adopted), 2048);
        assert_eq!(
            state.authority(MODEL).unwrap(),
            Authority::LegacyRecipeOptions
        );
    }
}
