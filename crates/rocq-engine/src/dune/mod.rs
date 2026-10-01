//! Thin Dune adapter for the canonical project view and build targets.
//!
//! This module is intentionally the only place that maps a logical library to
//! a project file.  Callers never supply a source path for a declaration.
use crate::types::{PetLoadPath, PetWorkspace};
use crate::{Error, ErrorKind, LogicalLibrary, Result};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

mod process;
pub(crate) use process::{NativeOutput, run_dune};

/// Bidirectional, unambiguous mapping for one Dune-reported project view.
#[derive(Eq, PartialEq)]
pub(crate) struct Layout {
    by_library: BTreeMap<LogicalLibrary, PathBuf>,
    by_file: BTreeMap<PathBuf, LogicalLibrary>,
    build_target_by_file: BTreeMap<PathBuf, PathBuf>,
    pet_workspace: PetWorkspace,
}

/// Dune-selected source ownership and reversible logical mappings for one workspace.
type DuneSourceLayout = (
    PathBuf,
    Vec<(PathBuf, LogicalLibrary, PathBuf, Vec<PetLoadPath>)>,
    Vec<(PathBuf, LogicalLibrary)>,
);

impl Layout {
    /// Builds a fail-closed logical layout from Dune's selected Rocq rules and
    /// returns Dune's canonical workspace root. Invalid mappings, unavailable
    /// sources, and Dune discovery exceeding `timeout` fail with typed errors.
    pub(crate) fn load(requested: &Path, timeout: Option<Duration>) -> Result<(PathBuf, Self)> {
        let (root, sources, mappings) = dune_sources(requested, timeout)?;
        let mut by_library = BTreeMap::new();
        let mut by_file = BTreeMap::new();
        let mut build_target_by_file = BTreeMap::new();
        let mut load_paths = Vec::new();
        for (directory, prefix) in mappings {
            let paths = sources
                .iter()
                .filter(|(path, mapping, _, _)| path.starts_with(&directory) && mapping == &prefix)
                .map(|(path, _, target, source_load_paths)| {
                    (path.clone(), target.clone(), source_load_paths.clone())
                })
                .collect::<Vec<_>>();
            for (path, build_target, source_load_paths) in paths {
                for mapping in &source_load_paths {
                    // A project `-R . Prefix` action is evaluated from Dune's
                    // build context.  The corresponding source-tree mapping
                    // is useful for selecting the logical unit above, but
                    // passing it to PET would make Coq search `_build` through
                    // the source root and expect names such as
                    // `Prefix._build.default.A`.  Keep compiled project
                    // mappings and external libraries; omit only the
                    // source-tree counterpart.
                    if pet_load_path_allowed(&root, mapping) && !load_paths.contains(mapping) {
                        load_paths.push(mapping.clone());
                    }
                }
                let canonical = fs::canonicalize(&path).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "project source is unavailable",
                    )
                })?;
                if !canonical.starts_with(&root) {
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
                    || by_file.insert(canonical.clone(), library).is_some()
                    || build_target_by_file
                        .insert(canonical.clone(), build_target)
                        .is_some()
                {
                    return Err(Error::new(
                        ErrorKind::Ambiguous,
                        "project layout is ambiguous",
                    ));
                }
            }
        }
        Ok((
            root.clone(),
            Self {
                by_library,
                by_file,
                build_target_by_file,
                pet_workspace: PetWorkspace { root, load_paths },
            },
        ))
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

    /// Whether Dune selected `file` as a Rocq compilation unit in this view.
    pub(crate) fn contains_file(&self, file: &Path) -> bool {
        self.by_file.contains_key(file)
    }

    /// Resolve the Dune-owned compilation unit that can define a canonical
    /// Rocq constant. The longest logical-library prefix wins; a constant
    /// outside the selected project returns `None` rather than being guessed
    /// from a filesystem path.
    pub(crate) fn source_for_constant(&self, constant: &str) -> Option<PathBuf> {
        let parts = constant.split('.').collect::<Vec<_>>();
        self.by_library
            .iter()
            .filter(|(library, _)| {
                parts.len() > library.0.len()
                    && parts
                        .iter()
                        .zip(&library.0)
                        .all(|(part, expected)| *part == expected)
            })
            .max_by_key(|(library, _)| library.0.len())
            .map(|(_, path)| path.clone())
    }

    /// Return the source-mapping root whose Dune-generated project file was
    /// materialized during attachment.
    pub(crate) fn pet_workspace(&self, file: &Path) -> Result<PetWorkspace> {
        self.by_file
            .contains_key(file)
            .then(|| self.pet_workspace.clone())
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "source is outside Dune project",
                )
            })
    }

    /// Returns the workspace-relative artifact target reported by Dune for a
    /// selected source file. No artifact extension or build-tree path is
    /// synthesized by the wrapper.
    pub(crate) fn build_target(&self, file: &Path) -> Result<PathBuf> {
        self.build_target_by_file.get(file).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune source has no reported build target",
            )
        })
    }
}

fn pet_load_path_allowed(root: &Path, mapping: &PetLoadPath) -> bool {
    if !mapping.physical.starts_with(root) {
        return true;
    }
    let build_root = match std::env::var_os("DUNE_BUILD_DIR") {
        Some(value) => {
            let value = PathBuf::from(value);
            if value.is_absolute() {
                value
            } else {
                root.join(value)
            }
        }
        None => root.join("_build"),
    };
    mapping.physical.starts_with(build_root)
}

/// Asks Dune for workspace metadata. The runner owns contention and process
/// deadlines; this adapter maps its native outcome to layout error classes.
fn dune_describe(
    workspace: &Path,
    args: &[&str],
    build_dir: Option<&Path>,
    timeout: Option<Duration>,
) -> Result<NativeOutput> {
    let output = run_dune(
        args.iter().map(OsString::from).collect::<Vec<_>>(),
        workspace,
        timeout,
        build_dir,
    )
    .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune is unavailable"))?;
    if output.cancelled {
        return Err(Error::new(
            ErrorKind::RequestCancelled,
            "Dune description was cancelled",
        ));
    }
    if output.timed_out {
        return Err(Error::new(
            ErrorKind::ProjectTimeout,
            "Dune description timed out",
        ));
    }
    if output.overflow {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune description exceeded output limit",
        ));
    }
    if !output.status.success() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            format!(
                "Dune {} failed: {}",
                args[1],
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(output)
}

/// Return the canonical workspace root and build-context path from one Dune
/// description. No dune-project/dune filename search is a second workspace
/// model.
fn dune_workspace(
    requested: &Path,
    build_dir: Option<&Path>,
    timeout: Option<Duration>,
) -> Result<(PathBuf, PathBuf)> {
    let output = dune_describe(
        requested,
        &["describe", "workspace", "--format", "sexp", "--lang", "0.1"],
        build_dir,
        timeout,
    )?;
    let description = String::from_utf8(output.stdout).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune workspace is not UTF-8",
        )
    })?;
    let tokens = sexp_tokens(&description)?;
    let root = tokens
        .windows(2)
        .find_map(|pair| (pair[0] == "root").then(|| PathBuf::from(&pair[1])))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune workspace root unavailable",
            )
        })?;
    let root = fs::canonicalize(root).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune workspace root unavailable",
        )
    })?;
    let build_context = tokens
        .windows(2)
        .find_map(|pair| (pair[0] == "build_context").then(|| root.join(&pair[1])))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune build context unavailable",
            )
        })?;
    Ok((root, build_context))
}

/// Ask Dune for the Rocq compilation rules it actually selected. The source
/// and logical prefix come from the same compiler action, so excluded trees,
/// generated targets, and `(modules ...)` need no separate scanner policy.
/// Errors if Dune cannot describe the workspace. This command does not compile
/// or mutate project sources.
// RISK: `dune describe rules` has documented S-expression output but no
// versioned schema. If Dune changes its action form, discovery fails closed.
fn dune_sources(requested: &Path, timeout: Option<Duration>) -> Result<DuneSourceLayout> {
    let deadline = timeout.and_then(|limit| Instant::now().checked_add(limit));
    // Design note: both queries share one private build tree. Neither reads
    // nor classifies the lock or mutable state of a concurrent project build.
    let isolated = tempfile::tempdir().map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune metadata directory unavailable",
        )
    })?;
    let build_dir = isolated.path().join("build");
    let (workspace, build_root) = dune_workspace(
        requested,
        Some(&build_dir),
        deadline.map(|limit| limit.saturating_duration_since(Instant::now())),
    )?;
    let args = ["describe", "rules"];
    let output = dune_describe(
        &workspace,
        &args,
        Some(&build_dir),
        deadline.map(|limit| limit.saturating_duration_since(Instant::now())),
    )?;
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
        // Design note: only a `.vo` target with a Rocq compile action owns a
        // source module. Other rules may mention `.v` merely as a dependency.
        if rule.iter().any(|token| token.ends_with(".vo"))
            && rule
                .windows(2)
                .any(|pair| pair[0].ends_with("rocq") && pair[1] == "compile")
        {
            // The dependency list also contains other `.vo` files.  Read the
            // `(targets ( ... (files (...) ...)))` subtree instead of relying
            // on token order: Dune is free to reorder dependencies and the
            // first/last `.vo` heuristic can silently assign a source to a
            // reverse dependency's artifact.
            let target = rule_target_vo(rule, &build_root).ok_or_else(|| {
                Error::new(ErrorKind::InvalidConfiguration, "Rocq rule lacks target")
            })?;
            let build_target = target
                .as_path()
                .strip_prefix(&build_root)
                .map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "Rocq target is outside Dune's build context",
                    )
                })?
                .to_owned();
            let action_dir = rule
                .windows(2)
                .find_map(|pair| (pair[0] == "chdir").then(|| PathBuf::from(&pair[1])))
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "Rocq compile action has no working directory",
                    )
                })?;
            let source_argument = rule
                .iter()
                .rev()
                .find(|token| token.ends_with(".v"))
                .ok_or_else(|| {
                    Error::new(ErrorKind::InvalidConfiguration, "Rocq rule lacks source")
                })?;
            let source_in_build = if Path::new(source_argument).is_absolute() {
                PathBuf::from(source_argument)
            } else {
                action_dir.join(source_argument)
            };
            let source =
                workspace.join(source_in_build.strip_prefix(&build_root).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "Rocq source is outside Dune's build context",
                    )
                })?);
            if source.starts_with(&workspace) && source.is_file() {
                let load_paths = dune_load_paths(rule, &action_dir, &workspace, &build_root)?;
                let mapping = load_paths
                    .iter()
                    .filter(|mapping| {
                        mapping.physical.starts_with(&workspace)
                            && source.starts_with(&mapping.physical)
                    })
                    .max_by_key(|mapping| mapping.physical.components().count())
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidConfiguration,
                            "Rocq rule lacks source mapping",
                        )
                    })?;
                let prefix = mapping.logical.clone();
                let directory = mapping.physical.clone();
                let source = fs::canonicalize(source).map_err(|_| {
                    Error::new(ErrorKind::InvalidConfiguration, "Dune source unavailable")
                })?;
                if !mappings.contains(&(directory.clone(), prefix.clone())) {
                    mappings.push((directory.clone(), prefix.clone()));
                }
                sources.push((source, prefix, build_target, load_paths));
            }
        }
        start = end;
    }
    Ok((workspace, sources, mappings))
}

/// Return the `.vo` explicitly listed by one Dune rule's `targets` form.
///
/// Dune's S-expression schema has remained stable enough for the surrounding
/// adapter to consume, but action/dependency contents are intentionally
/// untyped.  This helper therefore only trusts the structurally delimited
/// `targets/files` list and never scans the rule-wide token stream.
fn rule_target_vo(rule: &[String], build_root: &Path) -> Option<PathBuf> {
    let targets = rule.iter().position(|token| token == "targets")?;
    let targets_start = targets + 1;
    let targets_end = balanced_end(rule, targets_start)?;
    let files = (targets_start..targets_end).find(|&index| rule[index] == "files")?;
    let files_start = files + 1;
    let files_end = balanced_end(rule, files_start)?;
    rule[files_start..files_end]
        .iter()
        .filter(|token| token.ends_with(".vo"))
        .filter_map(|token| {
            let path = Path::new(token);
            if path.is_absolute() {
                path.starts_with(build_root).then(|| path.to_owned())
            } else {
                // Older Dune versions print paths relative to the build
                // context.  Normalize them to the same absolute form used by
                // `dune_workspace` before returning the target.
                Some(build_root.join(path))
            }
        })
        .next()
}

/// Find the exclusive closing parenthesis for a list beginning at `start`.
fn balanced_end(tokens: &[String], start: usize) -> Option<usize> {
    (tokens.get(start)? == "(").then_some(())?;
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        match token.as_str() {
            "(" => depth = depth.checked_add(1)?,
            ")" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Decode exactly the `-R`/`-Q` mappings in one Dune Rocq compile action.
/// Isolated metadata-build paths are translated to both the real Dune build
/// context and an existing source-tree counterpart. The latter is retained for
/// layout identity only; [`Layout::load`] excludes it from PET's compiled
/// library search path.
fn dune_load_paths(
    rule: &[String],
    action_dir: &Path,
    workspace: &Path,
    isolated_build_root: &Path,
) -> Result<Vec<PetLoadPath>> {
    let mut output = Vec::new();
    for triple in rule
        .windows(3)
        .filter(|triple| matches!(triple[0].as_str(), "-R" | "-Q"))
    {
        let logical = LogicalLibrary(triple[2].split('.').map(str::to_owned).collect());
        if logical
            .0
            .iter()
            .any(|part| validate_component(part).is_err())
        {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "invalid Dune Rocq load-path mapping",
            ));
        }
        let reported = if Path::new(&triple[1]).is_absolute() {
            PathBuf::from(&triple[1])
        } else {
            action_dir.join(&triple[1])
        };
        let reported = fs::canonicalize(&reported).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune Rocq load-path directory is unavailable",
            )
        })?;
        let implicit = triple[0] == "-R";
        if let Ok(relative) = reported.strip_prefix(isolated_build_root) {
            let source = workspace.join(relative);
            if source.is_dir() {
                let source = fs::canonicalize(source).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "Dune source load-path directory is unavailable",
                    )
                })?;
                // Dune compiles from its build-context copy. Retain the real
                // compiled root for PET and also return the source counterpart
                // below so layout discovery can assign a canonical file ID.
                let build_root = match std::env::var_os("DUNE_BUILD_DIR") {
                    Some(value) => {
                        let value = PathBuf::from(value);
                        if value.is_absolute() {
                            value
                        } else {
                            workspace.join(value)
                        }
                    }
                    None => workspace.join("_build"),
                };
                let compiled = build_root.join("default").join(relative);
                let compiled_mapping = PetLoadPath {
                    physical: compiled,
                    logical: logical.clone(),
                    implicit,
                };
                if !output.contains(&compiled_mapping) {
                    output.push(compiled_mapping);
                }
                let mapping = PetLoadPath {
                    physical: source,
                    logical: logical.clone(),
                    implicit,
                };
                if !output.contains(&mapping) {
                    output.push(mapping);
                }
            }
        } else {
            let mapping = PetLoadPath {
                physical: reported,
                logical,
                implicit,
            };
            if !output.contains(&mapping) {
                output.push(mapping);
            }
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

/// Bounded S-expression lexer for Dune metadata; it preserves quoted
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(value: &str) -> Vec<String> {
        sexp_tokens(value).expect("valid test S-expression")
    }

    #[test]
    fn target_mapping_ignores_dependency_vo_files() {
        let rule = tokens(
            "((deps ((File (In_build_dir /build/Other.vo)) (File (In_build_dir /build/Source.v)))) (targets ((files (/build/Source.glob /build/Source.vo)) (directories ()))) (action (run rocq compile Source.v)))",
        );
        assert_eq!(
            rule_target_vo(&rule, Path::new("/build")),
            Some(PathBuf::from("/build/Source.vo"))
        );
    }

    #[test]
    fn target_mapping_accepts_relative_build_context_paths() {
        let rule = tokens(
            "((deps ((File (In_build_dir Other.vo)))) (targets ((files (Source.vo Source.glob)) (directories ()))) (action (run rocq compile Source.v)))",
        );
        assert_eq!(
            rule_target_vo(&rule, Path::new("/build/default")),
            Some(PathBuf::from("/build/default/Source.vo"))
        );
    }

    #[test]
    fn pet_uses_compiled_project_mapping_not_source_root() {
        let logical = LogicalLibrary(vec!["Demo".into()]);
        let source = PetLoadPath {
            physical: PathBuf::from("/project"),
            logical: logical.clone(),
            implicit: true,
        };
        let build_root = match std::env::var_os("DUNE_BUILD_DIR") {
            Some(value) => {
                let value = PathBuf::from(value);
                if value.is_absolute() {
                    value
                } else {
                    Path::new("/project").join(value)
                }
            }
            None => PathBuf::from("/project/_build"),
        };
        let compiled = PetLoadPath {
            physical: build_root.join("default"),
            logical: logical.clone(),
            implicit: true,
        };
        let external = PetLoadPath {
            physical: PathBuf::from("/opt/rocq/lib"),
            logical,
            implicit: true,
        };
        assert!(!pet_load_path_allowed(Path::new("/project"), &source));
        assert!(pet_load_path_allowed(Path::new("/project"), &compiled));
        assert!(pet_load_path_allowed(Path::new("/project"), &external));
    }
}
