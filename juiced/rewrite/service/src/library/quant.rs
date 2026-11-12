use std::collections::BTreeMap;

const SEPARATORS: &[u8] = b"-._/";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileMeta {
    pub path: String,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Variant {
    pub label: Option<String>,
    pub primary: String,
    pub files: Vec<FileMeta>,
    pub parts: u32,
    pub complete: bool,
    /// Per-file artifact-set roles for companion bundles; empty for single files and splits.
    pub roles: Vec<crate::artifacts::ArtifactRole>,
}

impl Variant {
    pub fn bytes(&self) -> Option<u64> {
        self.files
            .iter()
            .try_fold(0u64, |total, file| total.checked_add(file.bytes?))
    }
    pub fn verifiable(&self) -> bool {
        self.complete
            && self
                .files
                .iter()
                .all(|file| file.bytes.is_some_and(|bytes| bytes > 0) && file.sha256.is_some())
    }
}

/// Files a catalog companion bundle may name; grouping still offers only model files.
pub fn is_listed_file(path: &str) -> bool {
    let name = basename(path).to_ascii_lowercase();
    is_model_file(path)
        || name.ends_with(".onnx")
        || name.ends_with(".ort")
        || name.ends_with(".safetensors")
        || name.ends_with(".bin")
        || matches!(name.as_str(), "streaming_config.json" | "tokenizer.json")
}

pub fn bundle(
    files: &[FileMeta],
    primary: &str,
    companions: &[(String, crate::artifacts::ArtifactRole)],
) -> Option<Variant> {
    let find = |path: &str| files.iter().find(|file| file.path == path).cloned();
    let mut members = vec![find(primary)?];
    let mut roles = vec![crate::artifacts::ArtifactRole::Model];
    let mut complete = true;
    for (path, role) in companions {
        match find(path) {
            Some(file) => {
                members.push(file);
                roles.push(*role);
            }
            None => complete = false,
        }
    }
    let label = label_for(primary).or_else(|| {
        let extension = basename(primary).rsplit_once('.')?.1;
        (!extension.is_empty() && extension.len() <= 16).then(|| extension.to_ascii_uppercase())
    });
    Some(Variant {
        label,
        primary: primary.to_owned(),
        files: members,
        parts: 1,
        complete,
        roles,
    })
}

pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn quant_label(name: &str) -> Option<String> {
    let upper = name.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    for start in 0..bytes.len() {
        if start > 0 && !SEPARATORS.contains(&bytes[start - 1]) {
            continue;
        }
        for end in candidates(bytes, start) {
            if end == bytes.len() || SEPARATORS.contains(&bytes[end]) {
                return Some(upper[start..end].to_owned());
            }
        }
    }
    None
}

fn candidates(bytes: &[u8], start: usize) -> Vec<usize> {
    let mut ends = Vec::new();
    let rest = &bytes[start..];
    if rest.starts_with(b"UD-") || rest.starts_with(b"UD_") {
        core(bytes, start + 3, &mut ends);
    }
    core(bytes, start, &mut ends);
    ends
}

fn digits(bytes: &[u8], at: usize) -> Option<usize> {
    let count = bytes[at.min(bytes.len())..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    (count > 0).then_some(at + count)
}

fn literal(bytes: &[u8], at: usize, text: &[u8]) -> Option<usize> {
    bytes
        .get(at..)
        .filter(|rest| rest.starts_with(text))
        .map(|_| at + text.len())
}

fn suffix(bytes: &[u8], at: usize, options: &[&[u8]]) -> Vec<usize> {
    let Some(after) = literal(bytes, at, b"_") else {
        return Vec::new();
    };
    let mut ends: Vec<usize> = options
        .iter()
        .filter_map(|option| literal(bytes, after, option))
        .collect();
    ends.sort_unstable_by(|left, right| right.cmp(left));
    ends
}

fn core(bytes: &[u8], at: usize, ends: &mut Vec<usize>) {
    if let Some(after) = literal(bytes, at, b"MXFP").and_then(|at| digits(bytes, at)) {
        if let Some(moe) = literal(bytes, after, b"_MOE") {
            ends.push(moe);
        }
        ends.push(after);
        return;
    }
    if let Some(end) = literal(bytes, at, b"BF16")
        .or_else(|| literal(bytes, at, b"F16"))
        .or_else(|| literal(bytes, at, b"F32"))
    {
        ends.push(end);
        return;
    }
    if let Some(after) = literal(bytes, at, b"IQ").and_then(|at| digits(bytes, at)) {
        ends.extend(suffix(
            bytes,
            after,
            &[b"XXS", b"XS", b"NL", b"M", b"S", b"L"],
        ));
        ends.push(after);
        return;
    }
    let Some(after) = literal(bytes, at, b"Q").and_then(|at| digits(bytes, at)) else {
        return;
    };
    let mut minor = Vec::new();
    if let Some(end) = literal(bytes, after, b"_").and_then(|at| {
        bytes
            .get(at)
            .filter(|byte| byte.is_ascii_digit())
            .map(|_| at + 1)
    }) {
        minor.push(end);
    }
    minor.push(after);
    for position in minor {
        let mut kinds = Vec::new();
        if let Some(end) = literal(bytes, position, b"_K") {
            kinds.push(end);
        }
        kinds.push(position);
        for kind in kinds {
            ends.extend(suffix(bytes, kind, &[b"XXL", b"XL", b"M", b"S", b"L"]));
            ends.push(kind);
        }
    }
}

pub fn split_part(path: &str) -> Option<(&str, u32, u32)> {
    let name = basename(path);
    let stem_length = name
        .len()
        .checked_sub(5)
        .filter(|length| name.as_bytes()[*length..].eq_ignore_ascii_case(b".gguf"))?;
    let stem = &name[..stem_length];
    let marker = stem.len().checked_sub(15)?;
    let tail = &stem.as_bytes()[marker..];
    if tail[0] != b'-'
        || &tail[6..10] != b"-of-"
        || !tail[1..6].iter().all(u8::is_ascii_digit)
        || !tail[10..15].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let index: u32 = stem[marker + 1..marker + 6].parse().ok()?;
    let total: u32 = stem[marker + 10..].parse().ok()?;
    if index == 0 || total == 0 || index > total {
        return None;
    }
    let prefix = path.len() - name.len() + marker;
    Some((&path[..prefix], index, total))
}

pub fn is_model_file(path: &str) -> bool {
    let name = basename(path).to_ascii_lowercase();
    if name.ends_with(".gguf") {
        return !name.contains("mmproj")
            && !name.starts_with("mtp-")
            && !name.contains("-mtp-ud-")
            && !name.starts_with("dflash-")
            && name != "dflash.gguf";
    }
    name.starts_with("ggml-") && name.ends_with(".bin")
}

pub fn stem(path: &str) -> String {
    let mut name = basename(path);
    if let Some((prefix, _, _)) = split_part(name) {
        name = prefix;
    } else if let Some(dot) = name.rfind('.') {
        let extension = &name[dot..];
        if extension.eq_ignore_ascii_case(".gguf") || extension.eq_ignore_ascii_case(".bin") {
            name = &name[..dot];
        }
    }
    let upper = name.to_ascii_uppercase();
    if let Some(label) = quant_label(name)
        && upper.ends_with(&label)
    {
        let cut = name.len() - label.len();
        if cut > 0 && SEPARATORS.contains(&name.as_bytes()[cut - 1]) {
            return name[..cut - 1].to_ascii_lowercase();
        }
    }
    name.to_ascii_lowercase()
}

fn label_for(path: &str) -> Option<String> {
    quant_label(basename(path)).or_else(|| quant_label(path))
}

pub fn group(files: &[FileMeta]) -> Vec<Variant> {
    let mut splits: BTreeMap<(String, u32), BTreeMap<u32, FileMeta>> = BTreeMap::new();
    let mut variants = Vec::new();
    for file in files.iter().filter(|file| is_model_file(&file.path)) {
        if let Some((prefix, index, total)) = split_part(&file.path) {
            splits
                .entry((prefix.to_owned(), total))
                .or_default()
                .entry(index)
                .or_insert_with(|| file.clone());
        } else {
            variants.push(Variant {
                label: label_for(&file.path),
                primary: file.path.clone(),
                files: vec![file.clone()],
                parts: 1,
                complete: true,
                roles: Vec::new(),
            });
        }
    }
    for ((_, total), parts) in splits {
        let complete = parts.len() == total as usize && parts.keys().copied().eq(1..=total);
        let files: Vec<FileMeta> = parts.into_values().collect();
        let primary = files[0].path.clone();
        variants.push(Variant {
            label: label_for(&primary),
            primary,
            files,
            parts: total,
            complete,
            roles: Vec::new(),
        });
    }
    variants.sort_by(|left, right| left.primary.cmp(&right.primary));
    variants
}
