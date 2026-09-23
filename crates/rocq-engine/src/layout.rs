//! Canonical project source layout derived from Rocq load-path declarations.
//!
//! This module is intentionally the only place that maps a logical library to
//! a project file.  Callers never supply a source path for a declaration.
use crate::{Error, ErrorKind, LogicalLibrary, Result};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

/// Bidirectional, unambiguous mapping for the current attached project view.
pub(crate) struct Layout {
    by_library: BTreeMap<LogicalLibrary, PathBuf>,
    by_file: BTreeMap<PathBuf, LogicalLibrary>,
    mappings: Vec<(PathBuf, LogicalLibrary)>,
    explicit_dune_modules: bool,
}

impl Layout {
    /// Builds a fail-closed logical layout from `_CoqProject` `-Q`/`-R` flags.
    /// `-I` is parsed for validity but does not define compilation-unit names.
    pub(crate) fn load(root: &Path, owned_paths: &[PathBuf]) -> Result<Self> {
        let mappings = coqproject_mappings(root)?;
        let mappings = if mappings.is_empty() {
            let dune = dune_mappings(root)?;
            if !dune.is_empty() {
                dune
            } else {
                // Design note: a project without explicit mappings has the sole
                // conventional root mapping, rather than guessing from filenames.
                vec![(root.to_owned(), LogicalLibrary(Vec::new()))]
            }
        } else {
            mappings
        };
        let mut by_library = BTreeMap::new();
        let mut by_file = BTreeMap::new();
        let mapping_roots = mappings.clone();
        let dune_modules = dune_modules(root)?;
        for (directory, prefix) in mappings {
            for entry in WalkDir::new(&directory).follow_links(false) {
                let entry = entry.map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "project layout traversal failed",
                    )
                })?;
                let path = entry.path();
                if ignored_tree(path, root) {
                    continue;
                }
                if owned_paths.iter().any(|owned| path.starts_with(owned)) {
                    continue;
                }
                if entry.file_type().is_symlink() {
                    continue;
                }
                if !entry.file_type().is_file() || path.extension().is_none_or(|x| x != "v") {
                    continue;
                }
                if let Some(modules) = dune_modules.get(&directory)
                    && !modules.is_empty()
                    && !path
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .is_some_and(|stem| modules.iter().any(|module| module == stem))
                {
                    continue;
                }
                let canonical = fs::canonicalize(path).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "project source is unavailable",
                    )
                })?;
                if !canonical.starts_with(root) {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "project layout escapes its root",
                    ));
                }
                let relative = canonical.strip_prefix(&directory).map_err(|_| {
                    Error::new(ErrorKind::InvalidConfiguration, "ambiguous project layout")
                })?;
                let mut parts = prefix.0.clone();
                let mut components = relative.components().peekable();
                while let Some(component) = components.next() {
                    let Component::Normal(value) = component else {
                        return Err(Error::new(
                            ErrorKind::InvalidConfiguration,
                            "unsafe project layout",
                        ));
                    };
                    let value = value.to_str().ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidConfiguration,
                            "project layout is not UTF-8",
                        )
                    })?;
                    if components.peek().is_none() {
                        let stem = Path::new(value)
                            .file_stem()
                            .and_then(|x| x.to_str())
                            .ok_or_else(|| {
                                Error::new(
                                    ErrorKind::InvalidConfiguration,
                                    "invalid Rocq source name",
                                )
                            })?;
                        validate_component(stem)?;
                        parts.push(stem.to_owned());
                    } else {
                        validate_component(value)?;
                        parts.push(value.to_owned());
                    }
                }
                let library = LogicalLibrary(parts);
                if by_library
                    .insert(library.clone(), canonical.clone())
                    .is_some()
                    || by_file.insert(canonical, library).is_some()
                {
                    return Err(Error::new(
                        ErrorKind::Ambiguous,
                        "project layout is ambiguous",
                    ));
                }
            }
        }
        let explicit_dune_modules =
            fs::read_to_string(root.join("dune"))
                .ok()
                .is_some_and(|source| {
                    source
                        .split(|x: char| x.is_whitespace() || matches!(x, '(' | ')'))
                        .any(|token| token == "modules")
                });
        Ok(Self {
            by_library,
            by_file,
            mappings: mapping_roots,
            explicit_dune_modules,
        })
    }

    pub(crate) fn target(&self, library: &LogicalLibrary) -> Result<(PathBuf, bool)> {
        if let Some(file) = self.by_library.get(library) {
            return Ok((file.clone(), self.explicit_dune_modules));
        }
        let mut candidates = self
            .mappings
            .iter()
            .filter(|(_, prefix)| library.0.starts_with(&prefix.0))
            .filter_map(|(directory, prefix)| {
                let suffix = &library.0[prefix.0.len()..];
                let (stem, parents) = suffix.split_last()?;
                let mut target = directory.clone();
                for parent in parents {
                    target.push(parent);
                }
                Some((prefix.0.len(), target.join(format!("{stem}.v"))))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(length, _)| *length);
        let Some((length, target)) = candidates.pop() else {
            return Err(Error::new(
                ErrorKind::NotFound,
                "new logical library has no reversible load path",
            ));
        };
        if candidates.last().is_some_and(|(other, _)| *other == length) {
            return Err(Error::new(
                ErrorKind::Ambiguous,
                "new logical library has ambiguous load paths",
            ));
        }
        Ok((target, self.explicit_dune_modules))
    }

    pub(crate) fn library(&self, file: &Path) -> Result<&LogicalLibrary> {
        self.by_file.get(file).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "source is outside the project layout",
            )
        })
    }

    pub(crate) fn files(&self) -> Vec<PathBuf> {
        self.by_file.keys().cloned().collect()
    }
}

/// Reads the supported `rocq.theory` Dune name form without treating arbitrary
/// Dune stanzas as source layout. Explicit `modules` remain an I2c update plan;
/// this tranche never edits Dune during declaration creation.
fn dune_mappings(root: &Path) -> Result<Vec<(PathBuf, LogicalLibrary)>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry
            .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune traversal failed"))?;
        if ignored_tree(entry.path(), root)
            || !entry.file_type().is_file()
            || entry.file_name() != "dune"
        {
            continue;
        }
        let source = fs::read_to_string(entry.path()).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune configuration is unavailable",
            )
        })?;
        let tokens = sexp_tokens(&source)?;
        let mut index = 0;
        while index + 1 < tokens.len() {
            if tokens[index] != "(" || tokens[index + 1] != "rocq.theory" {
                index += 1;
                continue;
            }
            let mut depth = 1;
            let mut end = index + 2;
            while end < tokens.len() && depth > 0 {
                if tokens[end] == "(" {
                    depth += 1;
                } else if tokens[end] == ")" {
                    depth -= 1;
                }
                end += 1;
            }
            if depth != 0 {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    "unterminated rocq.theory stanza",
                ));
            }
            let stanza = &tokens[index + 2..end - 1];
            let name = stanza
                .windows(2)
                .find_map(|pair| (pair[0] == "name").then(|| pair[1].clone()))
                .ok_or_else(|| {
                    Error::new(ErrorKind::InvalidConfiguration, "rocq.theory has no name")
                })?;
            let parts = name.split('.').map(str::to_owned).collect::<Vec<_>>();
            if parts.iter().any(|part| validate_component(part).is_err()) {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    "rocq.theory name is invalid",
                ));
            }
            if let Some(parent) = entry.path().parent() {
                out.push((parent.to_owned(), LogicalLibrary(parts)));
            }
            index = end;
        }
    }
    Ok(out)
}

/// Explicit Dune modules are local to the directory owning the rocq.theory
/// stanza; they are never treated as a root-wide filter.
fn dune_modules(root: &Path) -> Result<BTreeMap<PathBuf, Vec<String>>> {
    let mut out = BTreeMap::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry
            .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune traversal failed"))?;
        if ignored_tree(entry.path(), root)
            || !entry.file_type().is_file()
            || entry.file_name() != "dune"
        {
            continue;
        }
        let tokens = sexp_tokens(&fs::read_to_string(entry.path()).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune configuration is unavailable",
            )
        })?)?;
        let mut index = 0;
        while index + 1 < tokens.len() {
            if tokens[index] != "(" || tokens[index + 1] != "rocq.theory" {
                index += 1;
                continue;
            }
            let mut depth = 1;
            let mut end = index + 2;
            while end < tokens.len() && depth > 0 {
                if tokens[end] == "(" {
                    depth += 1;
                } else if tokens[end] == ")" {
                    depth -= 1;
                }
                end += 1;
            }
            let stanza = &tokens[index + 2..end.saturating_sub(1)];
            if let Some(position) = stanza.iter().position(|token| token == "modules") {
                let modules = stanza[position + 1..]
                    .iter()
                    .take_while(|token| *token != "name" && *token != "libraries")
                    .filter(|token| *token != "(")
                    .cloned()
                    .collect::<Vec<_>>();
                if let Some(parent) = entry.path().parent() {
                    out.insert(parent.to_owned(), modules);
                }
            }
            index = end;
        }
    }
    Ok(out)
}

fn coqproject_mappings(root: &Path) -> Result<Vec<(PathBuf, LogicalLibrary)>> {
    let path = root.join("_CoqProject");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let source = fs::read_to_string(path).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "_CoqProject is unavailable",
        )
    })?;
    let words = sexp_tokens(&source)?;
    let mut out = Vec::new();
    let mut index = 0;
    while index < words.len() {
        match words[index].as_str() {
            "-Q" | "-R" => {
                if index + 2 >= words.len() {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject load path is incomplete",
                    ));
                }
                let relative = Path::new(&words[index + 1]);
                if relative.components().any(|x| {
                    !matches!(
                        x,
                        Component::Normal(_) | Component::CurDir | Component::RootDir
                    )
                }) {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject load path is unsafe",
                    ));
                }
                let directory = fs::canonicalize(root.join(relative)).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject load path is unavailable",
                    )
                })?;
                if !directory.is_dir() {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject load path is unavailable",
                    ));
                }
                let library = words[index + 2]
                    .split('.')
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if library.iter().any(|x| validate_component(x).is_err()) {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject logical name is invalid",
                    ));
                }
                // External -Q/-R roots are compiler inputs, not project source
                // ownership.  They are authorized separately by the frozen
                // artifact baseline and must never enter source discovery.
                if directory.starts_with(root) {
                    out.push((directory, LogicalLibrary(library)));
                }
                index += 3;
            }
            "-I" => {
                if index + 1 >= words.len() {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject include path is incomplete",
                    ));
                }
                let include = Path::new(&words[index + 1]);
                let include_path = if include.is_absolute() {
                    include.to_owned()
                } else {
                    root.join(include)
                };
                if include.components().any(|x| {
                    !matches!(
                        x,
                        Component::Normal(_) | Component::CurDir | Component::RootDir
                    )
                }) || !fs::canonicalize(include_path).is_ok_and(|path| path.is_dir())
                {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject include path is unavailable",
                    ));
                }
                index += 2;
            }
            flag if flag.starts_with('-') => {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    "unsupported _CoqProject option",
                ));
            }
            _ => index += 1,
        }
    }
    Ok(out)
}

/// Returns compiler-owned roots outside the attached project.  They are never
/// indexed as editable source, but their `.vo` content is part of the trust
/// baseline and must be frozen before a candidate is solved.
pub(crate) fn external_load_paths(root: &Path) -> Result<Vec<(PathBuf, LogicalLibrary)>> {
    let path = root.join("_CoqProject");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let source = fs::read_to_string(path).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "_CoqProject is unavailable",
        )
    })?;
    let words = sexp_tokens(&source)?;
    let mut out = Vec::new();
    let mut index = 0;
    while index < words.len() {
        match words[index].as_str() {
            "-Q" | "-R" if index + 2 < words.len() => {
                let raw = Path::new(&words[index + 1]);
                let path = if raw.is_absolute() {
                    raw.to_owned()
                } else {
                    root.join(raw)
                };
                let path = fs::canonicalize(path).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject load path is unavailable",
                    )
                })?;
                let prefix =
                    LogicalLibrary(words[index + 2].split('.').map(str::to_owned).collect());
                if !path.starts_with(root) {
                    out.push((path, prefix));
                }
                index += 3;
            }
            "-I" if index + 1 < words.len() => {
                let raw = Path::new(&words[index + 1]);
                let path = if raw.is_absolute() {
                    raw.to_owned()
                } else {
                    root.join(raw)
                };
                let path = fs::canonicalize(path).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "_CoqProject include path is unavailable",
                    )
                })?;
                if !path.starts_with(root) {
                    out.push((path, LogicalLibrary(Vec::new())));
                }
                index += 2;
            }
            _ => index += 1,
        }
    }
    Ok(out)
}

/// Returns the already-validated load-path flags needed by direct `rocq
/// compile`. Source filenames and unsupported options are never forwarded.
pub(crate) fn compiler_options(root: &Path) -> Result<Vec<OsString>> {
    let path = root.join("_CoqProject");
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Reuse the authoritative validator before projecting tokens to argv.
    let _ = coqproject_mappings(root)?;
    let source = fs::read_to_string(path).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "_CoqProject is unavailable",
        )
    })?;
    let words = sexp_tokens(&source)?;
    let mut output = Vec::new();
    let mut index = 0usize;
    while index < words.len() {
        match words[index].as_str() {
            "-Q" | "-R" => {
                let directory = Path::new(&words[index + 1]);
                let directory = if directory.is_absolute() {
                    directory.to_owned()
                } else {
                    root.join(directory)
                };
                output.push(OsString::from(&words[index]));
                output.push(directory.into_os_string());
                output.push(OsString::from(&words[index + 2]));
                index += 3;
            }
            "-I" => {
                let directory = Path::new(&words[index + 1]);
                let directory = if directory.is_absolute() {
                    directory.to_owned()
                } else {
                    root.join(directory)
                };
                output.push(OsString::from("-I"));
                output.push(directory.into_os_string());
                index += 2;
            }
            _ => index += 1,
        }
    }
    Ok(output)
}

fn validate_component(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|x| x == '_' || x == '\'' || x.is_alphanumeric())
    {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "logical library component is invalid",
        ));
    }
    Ok(())
}

fn ignored_tree(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root).ok().is_some_and(|relative| {
        relative.components().any(|component| {
            let Component::Normal(value) = component else {
                return false;
            };
            matches!(
                value.to_str(),
                Some("_build" | ".git" | ".hg" | ".svn" | "target")
            )
        })
    })
}

/// Bounded S-expression lexer for Dune and `_CoqProject`; it preserves quoted
/// paths and rejects unterminated comments/strings/parentheses.
fn sexp_tokens(source: &str) -> Result<Vec<String>> {
    let chars = source.chars().collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut token = String::new();
    let mut quote = false;
    let mut comment = false;
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if comment {
            if c == '\n' {
                comment = false;
            }
            i += 1;
            continue;
        }
        if !quote && matches!(c, ';' | '#') {
            comment = true;
            i += 1;
            continue;
        }
        if c == '"' {
            quote = !quote;
            i += 1;
            continue;
        }
        if !quote && matches!(c, '(' | ')') {
            if !token.is_empty() {
                out.push(std::mem::take(&mut token));
            }
            out.push(c.to_string());
            if c == '(' {
                depth += 1;
            } else if depth == 0 {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    "unbalanced project expression",
                ));
            } else {
                depth -= 1;
            }
            i += 1;
            continue;
        }
        if !quote && c.is_whitespace() {
            if !token.is_empty() {
                out.push(std::mem::take(&mut token));
            }
            i += 1;
            continue;
        }
        token.push(c);
        i += 1;
    }
    if quote || depth != 0 {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "unterminated project expression",
        ));
    }
    if !token.is_empty() {
        out.push(token);
    }
    Ok(out)
}
