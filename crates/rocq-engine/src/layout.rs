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
    process::Command,
};
use walkdir::WalkDir;

/// Bidirectional, unambiguous mapping for the current attached project view.
pub(crate) struct Layout {
    by_library: BTreeMap<LogicalLibrary, PathBuf>,
    by_file: BTreeMap<PathBuf, LogicalLibrary>,
    mappings: Vec<(PathBuf, LogicalLibrary)>,
    dune_project: bool,
}

impl Layout {
    /// Builds a fail-closed logical layout. Dune projects use Dune's selected
    /// Rocq rules; other projects use `_CoqProject` load paths or the root.
    /// Owned paths are excluded; invalid mappings and unavailable inputs fail.
    pub(crate) fn load(root: &Path, owned_paths: &[PathBuf]) -> Result<Self> {
        let dune = dune_sources(root)?;
        let mappings = if let Some((_, mappings)) = &dune {
            mappings.clone()
        } else {
            let mappings = coqproject_mappings(root)?;
            if mappings.is_empty() {
                vec![(root.to_owned(), LogicalLibrary(Vec::new()))]
            } else {
                mappings
            }
        };
        let mut by_library = BTreeMap::new();
        let mut by_file = BTreeMap::new();
        let mapping_roots = mappings.clone();
        for (directory, prefix) in mappings {
            let paths = if let Some((sources, _)) = &dune {
                sources
                    .iter()
                    .filter(|(path, mapping)| path.starts_with(&directory) && mapping == &prefix)
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>()
            } else {
                let mut paths = Vec::new();
                for entry in WalkDir::new(&directory).follow_links(false) {
                    let entry = entry.map_err(|_| {
                        Error::new(
                            ErrorKind::InvalidConfiguration,
                            "project layout traversal failed",
                        )
                    })?;
                    let path = entry.path();
                    if entry.file_type().is_symlink() {
                        continue;
                    }
                    if !entry.file_type().is_file() || path.extension().is_none_or(|x| x != "v") {
                        continue;
                    }
                    // A non-Dune source must have a representable logical
                    // name; metadata directories with punctuation cannot.
                    let relative = path.strip_prefix(&directory).map_err(|_| {
                        Error::new(ErrorKind::InvalidConfiguration, "source escapes mapping")
                    })?;
                    let mut parts = relative.components().peekable();
                    let mut invalid = false;
                    while let Some(part) = parts.next() {
                        let Component::Normal(name) = part else {
                            invalid = true;
                            break;
                        };
                        let value = if parts.peek().is_none() {
                            Path::new(name).file_stem().and_then(|stem| stem.to_str())
                        } else {
                            name.to_str()
                        };
                        invalid |= value.is_none_or(|value| validate_component(value).is_err());
                    }
                    if invalid {
                        continue;
                    }
                    paths.push(path.to_owned());
                }
                paths
            };
            for path in paths {
                if owned_paths.iter().any(|owned| path.starts_with(owned)) {
                    continue;
                }
                let canonical = fs::canonicalize(&path).map_err(|_| {
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
        Ok(Self {
            by_library,
            by_file,
            mappings: mapping_roots,
            dune_project: dune.is_some(),
        })
    }

    /// Resolves an existing compilation unit or the unique reversible path
    /// for a new one. Ambiguous and unmapped logical names fail closed.
    pub(crate) fn target(&self, library: &LogicalLibrary) -> Result<PathBuf> {
        if let Some(file) = self.by_library.get(library) {
            return Ok(file.clone());
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
        Ok(target)
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

    pub(crate) fn is_dune_project(&self) -> bool {
        self.dune_project
    }
}

/// Returns Dune's build directory for a workspace, if one owns this path.
/// Errors on an unusable Dune installation or malformed workspace response.
pub(crate) fn dune_workspace_root(root: &Path) -> Option<&Path> {
    root.ancestors()
        .find(|dir| dir.join("dune-project").is_file())
}

pub(crate) fn dune_build_directory(root: &Path) -> Result<Option<PathBuf>> {
    let Some(workspace) = dune_workspace_root(root) else {
        return Ok(None);
    };
    let output = Command::new("dune")
        .current_dir(workspace)
        .args(["describe", "workspace", "--format", "sexp", "--lang", "0.1"])
        .output()
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune is unavailable"))?;
    if !output.status.success() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune workspace discovery failed",
        ));
    }
    let description = String::from_utf8(output.stdout).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune workspace is not UTF-8",
        )
    })?;
    let tokens = sexp_tokens(&description)?;
    let directory = tokens
        .windows(2)
        .find_map(|pair| (pair[0] == "build_context").then(|| workspace.join(&pair[1])))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune build context unavailable",
            )
        })?;
    Ok(Some(directory))
}

/// Ask Dune for the Rocq compilation rules it actually selected. The source
/// and logical prefix come from the same compiler action, so excluded trees,
/// generated targets, and `(modules ...)` need no separate scanner policy.
/// Returns `None` outside a Dune workspace and errors if Dune cannot describe
/// a workspace. This command does not compile or mutate project sources.
// RISK: `dune describe rules` has documented S-expression output but no
// versioned schema. If Dune changes its action form, discovery fails closed.
fn dune_sources(
    root: &Path,
) -> Result<
    Option<(
        Vec<(PathBuf, LogicalLibrary)>,
        Vec<(PathBuf, LogicalLibrary)>,
    )>,
> {
    let Some(workspace) = dune_workspace_root(root) else {
        return Ok(None);
    };
    let scope = root
        .strip_prefix(workspace)
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "invalid Dune scope"))?;
    let build_root = dune_build_directory(root)?.ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune build context unavailable",
        )
    })?;
    let build_root = build_root.strip_prefix(workspace).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune build directory escapes workspace",
        )
    })?;
    let mut command = Command::new("dune");
    command.current_dir(workspace).args(["describe", "rules"]);
    if !scope.as_os_str().is_empty() {
        command.arg(scope);
    }
    let output = command
        .output()
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune is unavailable"))?;
    if !output.status.success() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune rule discovery failed",
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune rules are not UTF-8"))?;
    let tokens = sexp_tokens(&text)?;
    let mut sources = Vec::new();
    let mut mappings = Vec::new();
    let mut start = 0;
    while start < tokens.len() {
        if tokens[start] != "(" {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "invalid Dune rule",
            ));
        }
        let mut depth = 0;
        let mut end = start;
        loop {
            if end >= tokens.len() {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    "unterminated Dune rule",
                ));
            }
            if tokens[end] == "(" {
                depth += 1;
            }
            if tokens[end] == ")" {
                depth -= 1;
            }
            end += 1;
            if depth == 0 {
                break;
            }
        }
        let rule = &tokens[start..end];
        // A theory without modules has no `.vo` rule, but its dependency rule
        // still declares the directory-to-logical-name mapping needed when a
        // new module is created.
        if let Some(target) = rule.iter().find(|token| token.ends_with(".theory.d")) {
            let build_dir = Path::new(target).parent().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "invalid Dune theory target",
                )
            })?;
            let source_dir = build_dir.strip_prefix(&build_root).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "invalid Dune theory context",
                )
            })?;
            let source_dir = fs::canonicalize(workspace.join(source_dir)).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Dune theory directory unavailable",
                )
            })?;
            if source_dir.starts_with(root) {
                let action_dir = rule
                    .windows(2)
                    .find_map(|pair| (pair[0] == "chdir").then(|| Path::new(&pair[1])))
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidConfiguration,
                            "Dune theory action directory unavailable",
                        )
                    })?;
                let action_dir = action_dir.strip_prefix(&build_root).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "invalid Dune theory action directory",
                    )
                })?;
                for pair in rule
                    .windows(3)
                    .filter(|triple| matches!(triple[0].as_str(), "-R" | "-Q"))
                {
                    let directory = fs::canonicalize(workspace.join(action_dir).join(&pair[1]));
                    if directory.as_ref().is_ok_and(|dir| dir == &source_dir) {
                        let prefix =
                            LogicalLibrary(pair[2].split('.').map(str::to_owned).collect());
                        if prefix
                            .0
                            .iter()
                            .any(|part| validate_component(part).is_err())
                        {
                            return Err(Error::new(
                                ErrorKind::InvalidConfiguration,
                                "invalid Dune theory mapping",
                            ));
                        }
                        if !mappings.contains(&(source_dir.clone(), prefix.clone())) {
                            mappings.push((source_dir.clone(), prefix));
                        }
                    }
                }
            }
        }
        // Design note: only a `.vo` target with a Rocq compile action owns a
        // source module. Other rules may mention `.v` merely as a dependency.
        if rule.iter().any(|token| token.ends_with(".vo"))
            && rule
                .windows(2)
                .any(|pair| pair[0].ends_with("rocq") && pair[1] == "compile")
        {
            let source = rule
                .iter()
                .rev()
                .find(|token| token.ends_with(".v"))
                .ok_or_else(|| {
                    Error::new(ErrorKind::InvalidConfiguration, "Rocq rule lacks source")
                })?;
            let source = workspace.join(source);
            if source.starts_with(root) && source.is_file() {
                let mapping = rule
                    .windows(3)
                    .filter(|triple| matches!(triple[0].as_str(), "-R" | "-Q"))
                    .filter_map(|triple| {
                        let dir = workspace.join(&triple[1]);
                        source.starts_with(&dir).then(|| (dir, triple[2].clone()))
                    })
                    .max_by_key(|(dir, _)| dir.components().count())
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidConfiguration,
                            "Rocq rule lacks source mapping",
                        )
                    })?;
                let prefix = LogicalLibrary(mapping.1.split('.').map(str::to_owned).collect());
                if prefix
                    .0
                    .iter()
                    .any(|part| validate_component(part).is_err())
                {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "invalid Dune Rocq mapping",
                    ));
                }
                let directory = fs::canonicalize(mapping.0).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "Dune source mapping unavailable",
                    )
                })?;
                let source = fs::canonicalize(source).map_err(|_| {
                    Error::new(ErrorKind::InvalidConfiguration, "Dune source unavailable")
                })?;
                if !mappings.contains(&(directory.clone(), prefix.clone())) {
                    mappings.push((directory.clone(), prefix.clone()));
                }
                sources.push((source, prefix));
            }
        }
        start = end;
    }
    Ok(Some((sources, mappings)))
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
