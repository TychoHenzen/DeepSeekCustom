//! Bounded repository indexing for Stage 1 localization.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use thiserror::Error;
use walkdir::{DirEntry, WalkDir};

use crate::config::settings::RepositoryIndexLimits;

use super::RepositoryIndexEntry;

const EXCLUDED_DIRECTORIES: &[&str] = &[".git", "target", ".deepseek"];
const BINARY_SCAN_BUFFER_BYTES: usize = 8 * 1024;
const BINARY_EXTENSIONS: &[&str] = &[
    "7z", "a", "avi", "bin", "bmp", "class", "dll", "dylib", "exe", "flac", "gif", "gz", "ico",
    "jar", "jpeg", "jpg", "lib", "mkv", "mov", "mp3", "mp4", "o", "obj", "onnx", "otf", "pdb",
    "pdf", "png", "pyc", "so", "tar", "ttf", "wav", "webm", "webp", "woff", "woff2", "xz", "zip",
];

/// Failure while constructing a bounded repository index.
#[derive(Debug, Error)]
pub enum RepositoryIndexError {
    #[error("repository index root is not a directory: {0}")]
    RootNotDirectory(PathBuf),
    #[error("could not walk repository index: {0}")]
    Walk(String),
    #[error("repository path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("could not read repository file {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("repository index file limit exceeded at {path}: configured max_files is {limit}")]
    MaxFilesExceeded { limit: usize, path: String },
    #[error(
        "repository index byte limit exceeded at {path}: {attempted} bytes would exceed configured max_total_bytes {limit}"
    )]
    MaxTotalBytesExceeded {
        limit: u64,
        attempted: u64,
        path: String,
    },
}

#[derive(Debug)]
struct Candidate {
    absolute_path: PathBuf,
    relative_path: String,
}

/// Walk `root` and return the deterministic, bounded localization index.
///
/// Paths use `/` separators and are relative to `root`. Generated state,
/// binary files, and links are absent from the result.
pub fn build_repository_index(
    root: &Path,
    limits: &RepositoryIndexLimits,
) -> Result<Vec<RepositoryIndexEntry>, RepositoryIndexError> {
    if !root.is_dir() {
        return Err(RepositoryIndexError::RootNotDirectory(root.to_path_buf()));
    }

    let mut candidates = collect_candidates(root)?;
    candidates.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let mut entries = Vec::new();
    let mut total_bytes = 0_u64;
    for candidate in candidates {
        let Some(source) = read_text_file(
            &candidate.absolute_path,
            &candidate.relative_path,
            total_bytes,
            limits.max_total_bytes,
        )?
        else {
            continue;
        };

        if entries.len() == limits.max_files {
            return Err(RepositoryIndexError::MaxFilesExceeded {
                limit: limits.max_files,
                path: candidate.relative_path,
            });
        }

        total_bytes = total_bytes
            .checked_add(source.len() as u64)
            .ok_or_else(|| RepositoryIndexError::MaxTotalBytesExceeded {
                limit: limits.max_total_bytes,
                attempted: u64::MAX,
                path: candidate.relative_path.clone(),
            })?;
        entries.push(RepositoryIndexEntry {
            path: candidate.relative_path,
            symbols: rust_symbols(&candidate.absolute_path, &source),
        });
    }
    Ok(entries)
}

fn rust_symbols(path: &Path, source: &str) -> Vec<String> {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
    {
        return Vec::new();
    }

    static ITEM_PATTERN: OnceLock<Regex> = OnceLock::new();
    static FUNCTION_PATTERN: OnceLock<Regex> = OnceLock::new();
    static MACRO_PATTERN: OnceLock<Regex> = OnceLock::new();
    let item_pattern = ITEM_PATTERN.get_or_init(|| {
        Regex::new(
            r"(?m)^[\t ]*(?:pub(?:[\t ]*\([^\n)]*\))?[\t ]+)?(?:struct|enum|trait|union|type|const|static|mod)[\t ]+([A-Za-z_][A-Za-z0-9_]*)\b",
        )
        .expect("repository item regex is valid")
    });
    let function_pattern = FUNCTION_PATTERN.get_or_init(|| {
        Regex::new(
            r"(?m)^[\t ]*(?:pub(?:[\t ]*\([^\n)]*\))?[\t ]+)?(?:(?:async|const|unsafe|default|extern)[\t ]+)*fn[\t ]+([A-Za-z_][A-Za-z0-9_]*)\b",
        )
        .expect("repository function regex is valid")
    });
    let macro_pattern = MACRO_PATTERN.get_or_init(|| {
        Regex::new(
            r"(?m)^[\t ]*(?:pub(?:[\t ]*\([^\n)]*\))?[\t ]+)?macro_rules![\t ]*([A-Za-z_][A-Za-z0-9_]*)\b",
        )
        .expect("repository macro regex is valid")
    });

    let searchable = mask_comments_and_literals(source);
    let mut symbols = Vec::new();
    for pattern in [item_pattern, function_pattern, macro_pattern] {
        symbols.extend(
            pattern
                .captures_iter(&searchable)
                .map(|capture| capture[1].to_string()),
        );
    }
    symbols.sort();
    symbols.dedup();
    symbols
}

#[derive(Clone, Copy)]
enum MaskState {
    Code,
    LineComment,
    BlockComment(usize),
    String { escaped: bool },
    Character { escaped: bool },
    RawString { hashes: usize },
}

fn mask_comments_and_literals(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut masked = bytes.to_vec();
    let mut state = MaskState::Code;
    let mut index = 0;

    while index < bytes.len() {
        match state {
            MaskState::Code => {
                if starts_with(bytes, index, b"//") {
                    mask_byte(&mut masked, index);
                    mask_byte(&mut masked, index + 1);
                    state = MaskState::LineComment;
                    index += 2;
                } else if starts_with(bytes, index, b"/*") {
                    mask_byte(&mut masked, index);
                    mask_byte(&mut masked, index + 1);
                    state = MaskState::BlockComment(1);
                    index += 2;
                } else if let Some((prefix_length, hashes)) = raw_string_start(bytes, index) {
                    for offset in 0..prefix_length {
                        mask_byte(&mut masked, index + offset);
                    }
                    state = MaskState::RawString { hashes };
                    index += prefix_length;
                } else if bytes[index] == b'"' {
                    mask_byte(&mut masked, index);
                    state = MaskState::String { escaped: false };
                    index += 1;
                } else if bytes[index] == b'\'' && is_character_literal(bytes, index) {
                    mask_byte(&mut masked, index);
                    state = MaskState::Character { escaped: false };
                    index += 1;
                } else {
                    index += 1;
                }
            }
            MaskState::LineComment => {
                if bytes[index] == b'\n' {
                    state = MaskState::Code;
                } else {
                    mask_byte(&mut masked, index);
                }
                index += 1;
            }
            MaskState::BlockComment(depth) => {
                if starts_with(bytes, index, b"/*") {
                    mask_byte(&mut masked, index);
                    mask_byte(&mut masked, index + 1);
                    state = MaskState::BlockComment(depth + 1);
                    index += 2;
                } else if starts_with(bytes, index, b"*/") {
                    mask_byte(&mut masked, index);
                    mask_byte(&mut masked, index + 1);
                    state = if depth == 1 {
                        MaskState::Code
                    } else {
                        MaskState::BlockComment(depth - 1)
                    };
                    index += 2;
                } else {
                    mask_byte(&mut masked, index);
                    index += 1;
                }
            }
            MaskState::String { escaped } => {
                let byte = bytes[index];
                mask_byte(&mut masked, index);
                if escaped {
                    state = MaskState::String { escaped: false };
                } else if byte == b'\\' {
                    state = MaskState::String { escaped: true };
                } else if byte == b'"' {
                    state = MaskState::Code;
                }
                index += 1;
            }
            MaskState::Character { escaped } => {
                let byte = bytes[index];
                mask_byte(&mut masked, index);
                if escaped {
                    state = MaskState::Character { escaped: false };
                } else if byte == b'\\' {
                    state = MaskState::Character { escaped: true };
                } else if byte == b'\'' {
                    state = MaskState::Code;
                }
                index += 1;
            }
            MaskState::RawString { hashes } => {
                if raw_string_end(bytes, index, hashes) {
                    mask_byte(&mut masked, index);
                    for offset in 0..hashes {
                        mask_byte(&mut masked, index + 1 + offset);
                    }
                    index += hashes + 1;
                    state = MaskState::Code;
                } else {
                    mask_byte(&mut masked, index);
                    index += 1;
                }
            }
        }
    }

    String::from_utf8(masked).expect("masking valid UTF-8 with ASCII spaces preserves UTF-8")
}

fn starts_with(bytes: &[u8], index: usize, needle: &[u8]) -> bool {
    bytes.get(index..index.saturating_add(needle.len())) == Some(needle)
}

fn mask_byte(masked: &mut [u8], index: usize) {
    if masked.get(index).is_some_and(|byte| *byte != b'\n') {
        masked[index] = b' ';
    }
}

fn raw_string_start(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    if index > 0 && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_') {
        return None;
    }

    let mut cursor = index;
    if matches!(bytes.get(cursor), Some(b'b' | b'c')) {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'"')).then_some((cursor + 1 - index, cursor - hashes_start))
}

fn raw_string_end(bytes: &[u8], index: usize, hashes: usize) -> bool {
    bytes.get(index) == Some(&b'"')
        && (0..hashes).all(|offset| bytes.get(index + 1 + offset) == Some(&b'#'))
}

fn is_character_literal(bytes: &[u8], index: usize) -> bool {
    let mut cursor = index + 1;
    let mut escaped = false;
    while cursor < bytes.len() && cursor <= index + 8 {
        let byte = bytes[cursor];
        if byte == b'\n' {
            return false;
        }
        if !escaped && byte == b'\'' {
            return cursor > index + 1;
        }
        escaped = !escaped && byte == b'\\';
        cursor += 1;
    }
    false
}

fn collect_candidates(root: &Path) -> Result<Vec<Candidate>, RepositoryIndexError> {
    let mut candidates = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(should_visit);

    for result in walker {
        let entry = result.map_err(|error| RepositoryIndexError::Walk(error.to_string()))?;
        if entry.depth() == 0 || !entry.file_type().is_file() || is_reparse_point(entry.path()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| RepositoryIndexError::Walk(error.to_string()))?;
        candidates.push(Candidate {
            absolute_path: entry.path().to_path_buf(),
            relative_path: normalized_relative_path(relative)?,
        });
    }
    Ok(candidates)
}

fn should_visit(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    if entry.file_type().is_symlink() || is_reparse_point(entry.path()) {
        return false;
    }
    if !entry.file_type().is_dir() {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !EXCLUDED_DIRECTORIES
        .iter()
        .any(|excluded| name.eq_ignore_ascii_case(excluded))
}

fn normalized_relative_path(path: &Path) -> Result<String, RepositoryIndexError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| RepositoryIndexError::NonUtf8Path(path.to_path_buf()))?,
            ),
            _ => {
                return Err(RepositoryIndexError::Walk(format!(
                    "repository path is not normalized: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

fn read_text_file(
    path: &Path,
    relative_path: &str,
    current_bytes: u64,
    max_total_bytes: u64,
) -> Result<Option<String>, RepositoryIndexError> {
    if has_binary_extension(path) {
        return Ok(None);
    }

    let metadata = fs::metadata(path).map_err(|source| RepositoryIndexError::Read {
        path: relative_path.to_string(),
        source,
    })?;
    let attempted = current_bytes.saturating_add(metadata.len());
    if attempted > max_total_bytes {
        if file_has_binary_content(path, relative_path)? {
            return Ok(None);
        }
        return Err(RepositoryIndexError::MaxTotalBytesExceeded {
            limit: max_total_bytes,
            attempted,
            path: relative_path.to_string(),
        });
    }

    let bytes = fs::read(path).map_err(|source| RepositoryIndexError::Read {
        path: relative_path.to_string(),
        source,
    })?;
    if is_binary_content(&bytes) {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

fn file_has_binary_content(path: &Path, relative_path: &str) -> Result<bool, RepositoryIndexError> {
    use std::io::Read;

    let mut file = fs::File::open(path).map_err(|source| RepositoryIndexError::Read {
        path: relative_path.to_string(),
        source,
    })?;
    let mut buffer = [0; BINARY_SCAN_BUFFER_BYTES];
    let mut incomplete_utf8 = Vec::new();
    loop {
        let length = file
            .read(&mut buffer)
            .map_err(|source| RepositoryIndexError::Read {
                path: relative_path.to_string(),
                source,
            })?;
        if length == 0 {
            return Ok(!incomplete_utf8.is_empty());
        }
        if buffer[..length].contains(&0) {
            return Ok(true);
        }

        incomplete_utf8.extend_from_slice(&buffer[..length]);
        match std::str::from_utf8(&incomplete_utf8) {
            Ok(_) => incomplete_utf8.clear(),
            Err(error) if error.error_len().is_some() => return Ok(true),
            Err(error) => {
                incomplete_utf8.drain(..error.valid_up_to());
            }
        }
    }
}

fn has_binary_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            BINARY_EXTENSIONS
                .iter()
                .any(|binary| extension.eq_ignore_ascii_case(binary))
        })
}

fn is_binary_content(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_reparse_point(_path: &Path) -> bool {
    false
}
