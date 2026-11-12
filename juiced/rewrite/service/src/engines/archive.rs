use super::catalog::ArchiveFormat;
use crate::runtime_bundle::{BundleFile, BundleSpec, FileRole};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub entries: usize,
    pub files: usize,
    pub file_bytes: u64,
    pub total_bytes: u64,
}

pub const LIMITS: Limits = Limits {
    entries: 8192,
    files: crate::runtime_bundle::MAX_FILES,
    file_bytes: crate::runtime_bundle::MAX_FILE_BYTES,
    total_bytes: crate::runtime_bundle::MAX_TOTAL_BYTES,
};

const XZ_MEMORY_LIMIT_KIB: u32 = 256 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_LINK_HOPS: usize = 16;

#[derive(Debug)]
pub enum ArchiveError {
    Io(io::Error),
    Corrupt,
    UnsafePath,
    LinkEscape,
    DirectoryLink,
    UnsupportedEntry,
    Duplicate,
    EntryLimit,
    SizeLimit,
    Cancelled,
    EntrypointMissing,
    Layout,
}

impl ArchiveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "archive_io",
            Self::Corrupt => "archive_corrupt",
            Self::UnsafePath => "archive_unsafe_path",
            Self::LinkEscape => "archive_link_escape",
            Self::DirectoryLink => "archive_directory_link",
            Self::UnsupportedEntry => "archive_unsupported_entry",
            Self::Duplicate => "archive_duplicate_entry",
            Self::EntryLimit => "archive_entry_limit",
            Self::SizeLimit => "archive_size_limit",
            Self::Cancelled => "cancelled",
            Self::EntrypointMissing => "archive_entrypoint_missing",
            Self::Layout => "archive_layout_unsupported",
        }
    }
}

impl From<io::Error> for ArchiveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Extracted {
    pub files: usize,
    pub bytes: u64,
    pub aliases_materialized: usize,
    pub aliases_dropped: usize,
}

enum Kind {
    File { executable: bool, size: u64 },
    Directory,
    Symlink(String),
    Hardlink(String),
    Ignored,
}

pub fn normalize(raw: &str) -> Result<Option<PathBuf>, ArchiveError> {
    if raw.is_empty()
        || raw.len() > 4096
        || raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
    {
        return Err(ArchiveError::UnsafePath);
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(ArchiveError::UnsafePath),
            name => parts.push(name),
        }
    }
    if parts.len() > MAX_DEPTH {
        return Err(ArchiveError::UnsafePath);
    }
    Ok((!parts.is_empty()).then(|| parts.iter().collect()))
}

fn resolve_symlink(link: &Path, target: &str) -> Result<PathBuf, ArchiveError> {
    if target.is_empty()
        || target.len() > 4096
        || target.starts_with('/')
        || target.contains('\\')
        || target.chars().any(char::is_control)
    {
        return Err(ArchiveError::LinkEscape);
    }
    let mut parts: Vec<&std::ffi::OsStr> = link
        .parent()
        .map(|parent| parent.iter().collect())
        .unwrap_or_default();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or(ArchiveError::LinkEscape)?;
            }
            name => parts.push(std::ffi::OsStr::new(name)),
        }
    }
    if parts.is_empty() || parts.len() > MAX_DEPTH {
        return Err(ArchiveError::LinkEscape);
    }
    Ok(parts.iter().collect())
}

struct Writer<'a> {
    root: &'a Path,
    limits: &'a Limits,
    cancelled: &'a AtomicBool,
    entries: usize,
    seen: BTreeSet<PathBuf>,
    files: BTreeSet<PathBuf>,
    directories: BTreeSet<PathBuf>,
    links: Vec<(PathBuf, PathBuf)>,
    extracted: Extracted,
}

impl<'a> Writer<'a> {
    fn new(root: &'a Path, limits: &'a Limits, cancelled: &'a AtomicBool) -> Self {
        Self {
            root,
            limits,
            cancelled,
            entries: 0,
            seen: BTreeSet::new(),
            files: BTreeSet::new(),
            directories: BTreeSet::new(),
            links: Vec::new(),
            extracted: Extracted::default(),
        }
    }

    fn check(&self) -> Result<(), ArchiveError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(ArchiveError::Cancelled);
        }
        Ok(())
    }

    fn ensure_directory(&mut self, relative: &Path) -> Result<(), ArchiveError> {
        let mut current = PathBuf::new();
        for part in relative.components() {
            let Component::Normal(name) = part else {
                return Err(ArchiveError::UnsafePath);
            };
            current.push(name);
            if self.files.contains(&current) {
                return Err(ArchiveError::Duplicate);
            }
            if self.directories.contains(&current) {
                continue;
            }
            let path = self.root.join(&current);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => return Err(ArchiveError::UnsafePath),
                Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&path)?,
                Err(error) => return Err(error.into()),
            }
            self.directories.insert(current.clone());
        }
        Ok(())
    }

    fn accept(
        &mut self,
        raw: &str,
        kind: Kind,
        content: &mut dyn Read,
    ) -> Result<(), ArchiveError> {
        self.check()?;
        self.entries += 1;
        if self.entries > self.limits.entries {
            return Err(ArchiveError::EntryLimit);
        }
        let Some(path) = normalize(raw)? else {
            return match kind {
                Kind::Directory | Kind::Ignored => Ok(()),
                _ => Err(ArchiveError::UnsafePath),
            };
        };
        match kind {
            Kind::Ignored => Ok(()),
            Kind::Directory => {
                if self.seen.contains(&path) {
                    return Err(ArchiveError::Duplicate);
                }
                self.ensure_directory(&path)
            }
            Kind::Symlink(target) => {
                let target = resolve_symlink(&path, &target)?;
                self.link(path, target)
            }
            Kind::Hardlink(target) => {
                let target = normalize(&target)
                    .map_err(|_| ArchiveError::LinkEscape)?
                    .ok_or(ArchiveError::LinkEscape)?;
                self.link(path, target)
            }
            Kind::File { executable, size } => {
                if !self.seen.insert(path.clone()) || self.directories.contains(&path) {
                    return Err(ArchiveError::Duplicate);
                }
                self.extracted.files += 1;
                if self.extracted.files > self.limits.files || size > self.limits.file_bytes {
                    return Err(if size > self.limits.file_bytes {
                        ArchiveError::SizeLimit
                    } else {
                        ArchiveError::EntryLimit
                    });
                }
                let total = self
                    .extracted
                    .bytes
                    .checked_add(size)
                    .filter(|total| *total <= self.limits.total_bytes)
                    .ok_or(ArchiveError::SizeLimit)?;
                if let Some(parent) = path.parent() {
                    self.ensure_directory(parent)?;
                }
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(if executable { 0o755 } else { 0o644 });
                }
                #[cfg(not(unix))]
                let _ = executable;
                let mut file = options.open(self.root.join(&path))?;
                let mut remaining = size;
                let mut buffer = vec![0; 1024 * 1024];
                loop {
                    self.check()?;
                    let read = content
                        .read(&mut buffer)
                        .map_err(|_| ArchiveError::Corrupt)?;
                    if read == 0 {
                        break;
                    }
                    if read as u64 > remaining {
                        return Err(ArchiveError::SizeLimit);
                    }
                    file.write_all(&buffer[..read])?;
                    remaining -= read as u64;
                }
                if remaining != 0 {
                    return Err(ArchiveError::Corrupt);
                }
                file.sync_all()?;
                self.files.insert(path);
                self.extracted.bytes = total;
                Ok(())
            }
        }
    }

    fn link(&mut self, path: PathBuf, target: PathBuf) -> Result<(), ArchiveError> {
        if !self.seen.insert(path.clone()) || self.directories.contains(&path) {
            return Err(ArchiveError::Duplicate);
        }
        self.links.push((path, target));
        Ok(())
    }

    /// The loader resolves libraries by SONAME (`libfoo.so.N`), so each aliased file is
    /// stored once under its shortest versioned name; development and full-version
    /// aliases are dropped instead of being duplicated.
    fn materialize_aliases(&mut self) -> Result<(), ArchiveError> {
        let links: BTreeMap<_, _> = self.links.iter().cloned().collect();
        let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for (alias, first) in &self.links {
            let mut target = first.clone();
            let mut hops = 0;
            while let Some(next) = links.get(&target) {
                hops += 1;
                if hops > MAX_LINK_HOPS {
                    return Err(ArchiveError::LinkEscape);
                }
                target = next.clone();
            }
            if self.directories.contains(&target) {
                return Err(ArchiveError::DirectoryLink);
            }
            if self.files.contains(&target) {
                groups.entry(target).or_default().push(alias.clone());
            } else {
                self.extracted.aliases_dropped += 1;
            }
        }
        for (file, aliases) in groups {
            let keep = std::iter::once(&file)
                .chain(aliases.iter())
                .filter_map(|name| {
                    soname_rank(name).map(|rank| (rank, name.as_os_str().len(), name))
                })
                .min()
                .map_or(&file, |(_, _, name)| name)
                .clone();
            let renamed = keep != file;
            if renamed {
                if let Some(parent) = keep.parent() {
                    self.ensure_directory(parent)?;
                }
                fs::rename(self.root.join(&file), self.root.join(&keep))?;
                self.files.remove(&file);
                self.files.insert(keep);
                self.extracted.aliases_materialized += 1;
            }
            self.extracted.aliases_dropped += aliases.len() - usize::from(renamed);
        }
        Ok(())
    }
}

fn soname_rank(path: &Path) -> Option<usize> {
    let name = path.file_name()?.to_str()?;
    let index = name.find(".so.")?;
    let version = &name[index + 4..];
    (!version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && !version.starts_with('.')
        && !version.ends_with('.'))
    .then(|| version.matches('.').count())
}

pub fn extract(
    archive: &Path,
    format: ArchiveFormat,
    destination: &Path,
    limits: &Limits,
    cancelled: &AtomicBool,
) -> Result<Extracted, ArchiveError> {
    if fs::read_dir(destination)?.next().is_some() {
        return Err(ArchiveError::UnsafePath);
    }
    let mut writer = Writer::new(destination, limits, cancelled);
    let input = BufReader::with_capacity(1024 * 1024, File::open(archive)?);
    match format {
        ArchiveFormat::TarGz => read_tar(flate2::read::GzDecoder::new(input), &mut writer)?,
        ArchiveFormat::TarXz => read_tar(
            lzma_rust2::XzReader::new_mem_limit(input, true, XZ_MEMORY_LIMIT_KIB),
            &mut writer,
        )?,
        ArchiveFormat::Zip => read_zip(File::open(archive)?, &mut writer)?,
    }
    writer.materialize_aliases()?;
    Ok(writer.extracted)
}

fn read_tar(input: impl Read, writer: &mut Writer<'_>) -> Result<(), ArchiveError> {
    let mut archive = tar::Archive::new(input);
    for entry in archive.entries().map_err(|_| ArchiveError::Corrupt)? {
        let mut entry = entry.map_err(|_| ArchiveError::Corrupt)?;
        let raw = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| ArchiveError::UnsafePath)?;
        let link = || -> Result<String, ArchiveError> {
            entry
                .link_name_bytes()
                .map(|bytes| String::from_utf8(bytes.into_owned()))
                .ok_or(ArchiveError::Corrupt)?
                .map_err(|_| ArchiveError::LinkEscape)
        };
        let header = entry.header();
        let kind = match header.entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => Kind::File {
                executable: header.mode().map_err(|_| ArchiveError::Corrupt)? & 0o111 != 0,
                size: entry.size(),
            },
            tar::EntryType::Directory => Kind::Directory,
            tar::EntryType::Symlink => Kind::Symlink(link()?),
            tar::EntryType::Link => Kind::Hardlink(link()?),
            tar::EntryType::XGlobalHeader => Kind::Ignored,
            _ => return Err(ArchiveError::UnsupportedEntry),
        };
        writer.accept(&raw, kind, &mut entry)?;
    }
    Ok(())
}

fn read_zip(input: File, writer: &mut Writer<'_>) -> Result<(), ArchiveError> {
    let mut archive = zip::ZipArchive::new(input).map_err(|_| ArchiveError::Corrupt)?;
    if archive.len() > writer.limits.entries {
        return Err(ArchiveError::EntryLimit);
    }
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| ArchiveError::Corrupt)?;
        let raw = entry
            .name()
            .map_err(|_| ArchiveError::UnsafePath)?
            .into_owned();
        let mode = entry.unix_mode().unwrap_or(0);
        let kind = if entry.is_dir() {
            Kind::Directory
        } else if mode & 0o170000 == 0o120000 {
            if entry.size() > 4096 {
                return Err(ArchiveError::LinkEscape);
            }
            let mut target = String::new();
            entry
                .by_ref()
                .take(4097)
                .read_to_string(&mut target)
                .map_err(|_| ArchiveError::LinkEscape)?;
            Kind::Symlink(target)
        } else if mode & 0o170000 == 0 || mode & 0o170000 == 0o100000 {
            Kind::File {
                executable: mode & 0o111 != 0,
                size: entry.size(),
            }
        } else {
            return Err(ArchiveError::UnsupportedEntry);
        };
        writer.accept(&raw, kind, &mut entry)?;
    }
    Ok(())
}

pub struct Described {
    pub bundle: BundleSpec,
    pub sizes: Vec<(PathBuf, u64)>,
}

fn shared_library(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".so") || soname_rank(Path::new(name)).is_some())
}

pub fn bundle_root(tree: &Path) -> Result<PathBuf, ArchiveError> {
    let mut entries = fs::read_dir(tree)?;
    let first = entries.next().transpose()?;
    if let Some(first) = first
        && entries.next().is_none()
        && first.file_type()?.is_dir()
    {
        return Ok(first.path());
    }
    Ok(tree.to_owned())
}

pub fn describe(
    root: &Path,
    engine: super::catalog::Engine,
    limits: &Limits,
    cancelled: &AtomicBool,
) -> Result<Described, ArchiveError> {
    let mut pending = vec![PathBuf::new()];
    let mut files = Vec::new();
    let mut entries = 0usize;
    while let Some(relative) = pending.pop() {
        for item in fs::read_dir(root.join(&relative))? {
            if cancelled.load(Ordering::Acquire) {
                return Err(ArchiveError::Cancelled);
            }
            entries += 1;
            if entries > limits.entries {
                return Err(ArchiveError::EntryLimit);
            }
            let item = item?;
            let name = item.file_name();
            let path = relative.join(&name);
            let metadata = fs::symlink_metadata(item.path())?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                files.push((path, metadata.len(), executable));
            } else {
                return Err(ArchiveError::UnsupportedEntry);
            }
        }
    }
    if files.len() > limits.files {
        return Err(ArchiveError::EntryLimit);
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let wanted = engine.entrypoint();
    let candidates: Vec<_> = files
        .iter()
        .filter(|(path, bytes, _)| {
            *bytes > 0 && path.file_name().is_some_and(|name| name == wanted)
        })
        .map(|(path, _, _)| path.clone())
        .collect();
    let depth = candidates
        .iter()
        .map(|path| path.components().count())
        .min()
        .ok_or(ArchiveError::EntrypointMissing)?;
    let shallow: Vec<_> = candidates
        .into_iter()
        .filter(|path| path.components().count() == depth)
        .collect();
    let [entrypoint] = shallow.as_slice() else {
        return Err(ArchiveError::Layout);
    };
    let mut library_directories = BTreeSet::new();
    library_directories.insert(directory_of(entrypoint));
    let mut bundle_files = Vec::with_capacity(files.len());
    let mut sizes = Vec::with_capacity(files.len());
    for (path, bytes, executable) in &files {
        let role = if *bytes == 0 {
            FileRole::Resource
        } else if path == entrypoint {
            FileRole::Executable
        } else if shared_library(path) {
            library_directories.insert(directory_of(path));
            FileRole::Library
        } else if *executable {
            FileRole::Executable
        } else {
            FileRole::Resource
        };
        bundle_files.push(BundleFile {
            path: path.clone(),
            sha256: file_sha256(&root.join(path), cancelled)?,
            role,
        });
        sizes.push((path.clone(), *bytes));
    }
    if library_directories.len() > 16 {
        return Err(ArchiveError::Layout);
    }
    Ok(Described {
        bundle: BundleSpec {
            schema_version: 1,
            implementation_id: engine
                .provider()
                .implementation_id()
                .expect("concrete engine")
                .to_owned(),
            root: root.to_owned(),
            entrypoint: entrypoint.clone(),
            library_directories: library_directories.into_iter().collect(),
            files: bundle_files,
        },
        sizes,
    })
}

fn directory_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_owned(),
        _ => PathBuf::from("."),
    }
}

pub fn file_sha256(path: &Path, cancelled: &AtomicBool) -> Result<String, ArchiveError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(ArchiveError::Cancelled);
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
