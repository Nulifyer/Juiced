//! Split GGUF models. llama.cpp opens the first part and derives every other part's file name
//! from it, so the names must keep the exact `-00001-of-00003.gguf` spelling.

use std::{fs::File, path::Path};

/// Artifact sets hold at most 64 members, and one may be a projector.
pub const MAXIMUM_PARTS: u32 = 63;

/// `(prefix, part, parts)` for `{prefix}-{part:05}-of-{parts:05}.gguf` with more than one part.
pub fn parse(name: &str) -> Option<(&str, u32, u32)> {
    let stem = name.strip_suffix(".gguf")?;
    let marker = stem.len().checked_sub(15)?;
    if !stem.is_char_boundary(marker) {
        return None;
    }
    let (prefix, tail) = stem.split_at(marker);
    let tail = tail.as_bytes();
    if prefix.is_empty()
        || tail[0] != b'-'
        || &tail[6..10] != b"-of-"
        || !tail[1..6].iter().all(u8::is_ascii_digit)
        || !tail[10..15].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let part: u32 = stem[marker + 1..marker + 6].parse().ok()?;
    let parts: u32 = stem[marker + 11..].parse().ok()?;
    (parts > 1 && part >= 1 && part <= parts).then_some((prefix, part, parts))
}

pub fn name(prefix: &str, part: u32, parts: u32) -> String {
    format!("{prefix}-{part:05}-of-{parts:05}.gguf")
}

#[derive(Debug, PartialEq, Eq)]
pub enum PartError {
    Unreadable,
    /// The header does not say it is this part of this many.
    Mismatch,
}

/// The part count a GGUF header declares, when it declares more than one.
pub fn declared_parts(file: &mut File) -> Result<Option<u64>, PartError> {
    let split = crate::profiles::read_gguf_split(file).map_err(|_| PartError::Unreadable)?;
    Ok(split.count.filter(|count| *count > 1))
}

pub fn verify(file: &mut File, part: u32, parts: u32) -> Result<(), PartError> {
    let split = crate::profiles::read_gguf_split(file).map_err(|_| PartError::Unreadable)?;
    if split.number != Some(u64::from(part) - 1) || split.count != Some(u64::from(parts)) {
        return Err(PartError::Mismatch);
    }
    Ok(())
}

pub fn verify_path(path: &Path, part: u32, parts: u32) -> Result<(), PartError> {
    let mut file = File::open(path).map_err(|_| PartError::Unreadable)?;
    verify(&mut file, part, parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_llama_cpp_spelling_names_a_part() {
        assert_eq!(
            parse("Qwen3-30B-Q4_K_M-00001-of-00003.gguf"),
            Some(("Qwen3-30B-Q4_K_M", 1, 3))
        );
        assert_eq!(parse("m-00002-of-00002.gguf"), Some(("m", 2, 2)));
        for refused in [
            "single-00001-of-00001.gguf",
            "model-00002-OF-00002.gguf",
            "model-00001-of-00002.GGUF",
            "model-1-of-3.gguf",
            "-00001-of-00002.gguf",
            "model-00003-of-00002.gguf",
            "model-00000-of-00002.gguf",
            "plain.gguf",
        ] {
            assert_eq!(parse(refused), None, "{refused}");
        }
        assert_eq!(name("m", 2, 3), "m-00002-of-00003.gguf");
    }
}
