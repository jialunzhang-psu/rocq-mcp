//! Thin Dune adapter for the canonical project view and build targets.
//!
//! This module is intentionally the only place that maps a logical library to
//! a project file.  Callers never supply a source path for a declaration.
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

#[cfg(test)]
mod tests;

/// Bidirectional, unambiguous mapping for the current attached project view.
pub(crate) struct Layout {
    workspace_root: PathBuf,
    by_library: BTreeMap<LogicalLibrary, PathBuf>,
    by_file: BTreeMap<PathBuf, LogicalLibrary>,
    build_target_by_file: BTreeMap<PathBuf, PathBuf>,
    pet_workspace_by_file: BTreeMap<PathBuf, PetWorkspace>,
    inputs: Vec<PathBuf>,
}

/// One source mapping root and Dune's optional generated PET project target.
#[derive(Clone, Debug)]
struct PetWorkspace {
    root: PathBuf,
    project_target: Option<PathBuf>,
}

/// Dune-selected source ownership and reversible logical mappings for one workspace.
type DuneSourceLayout = (
    PathBuf,
    Vec<(PathBuf, LogicalLibrary, PathBuf)>,
    Vec<(PathBuf, LogicalLibrary)>,
    Vec<PathBuf>,
    Vec<(PathBuf, PathBuf)>,
);

impl Layout {
    /// Builds a fail-closed logical layout from Dune's selected Rocq rules.
    /// Owned paths are excluded; invalid mappings, unavailable inputs, and
    /// Dune discovery exceeding `timeout` fail with typed errors.
    pub(crate) fn load(root: &Path, owned_paths: &[PathBuf], timeout: Duration) -> Result<Self> {
        let (workspace_root, sources, mappings, inputs, pet_projects) =
            dune_sources(root, timeout)?;
        let mut by_library = BTreeMap::new();
        let mut by_file = BTreeMap::new();
        let mut build_target_by_file = BTreeMap::new();
        let mut pet_workspace_by_file = BTreeMap::new();
        for (directory, prefix) in mappings {
            let paths = sources
                .iter()
                .filter(|(path, mapping, _)| path.starts_with(&directory) && mapping == &prefix)
                .map(|(path, _, target)| (path.clone(), target.clone()))
                .collect::<Vec<_>>();
            for (path, build_target) in paths {
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
                    || by_file.insert(canonical.clone(), library).is_some()
                    || build_target_by_file
                        .insert(canonical.clone(), build_target)
                        .is_some()
                    || pet_workspace_by_file
                        .insert(
                            canonical,
                            PetWorkspace {
                                root: directory.clone(),
                                project_target: pet_projects.iter().find_map(|(root, target)| {
                                    (root == &directory).then(|| target.clone())
                                }),
                            },
                        )
                        .is_some()
                {
                    return Err(Error::new(
                        ErrorKind::Ambiguous,
                        "project layout is ambiguous",
                    ));
                }
            }
        }
        Ok(Self {
            workspace_root,
            by_library,
            by_file,
            build_target_by_file,
            pet_workspace_by_file,
            inputs,
        })
    }

    /// Resolves an existing Dune-selected compilation unit.
    pub(crate) fn target(&self, library: &LogicalLibrary) -> Result<PathBuf> {
        self.by_library.get(library).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                "logical library is not selected by Dune",
            )
        })
    }

    /// Return the canonical workspace that owns every reported target.
    pub(crate) fn workspace_root(&self) -> &Path {
        &self.workspace_root
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
        let exact = self
            .by_library
            .iter()
            .filter(|(library, _)| {
                parts.len() > library.0.len()
                    && parts
                        .iter()
                        .zip(&library.0)
                        .all(|(part, expected)| *part == expected)
            })
            .max_by_key(|(library, _)| library.0.len())
            .map(|(_, path)| path.clone());
        if exact.is_some() {
            return exact;
        }

        // PET can legitimately expose a source-relative constant when no
        // generated `_RocqProject` is available (for example `Main.t` for a
        // Dune logical library `RejectPubDune.Main`).  Recover ownership from
        // Dune's reported compilation-unit suffix, not from a global
        // name-only declaration catalogue.  Require a unique longest suffix;
        // ambiguous files fail closed and are never scanned speculatively.
        let mut candidates = self
            .by_library
            .iter()
            .filter_map(|(library, path)| {
                let max = library.0.len().min(parts.len().saturating_sub(1));
                (1..=max)
                    .rev()
                    .find(|length| library.0[library.0.len() - length..] == parts[..*length])
                    .map(|length| (length, path.clone()))
            })
            .collect::<Vec<_>>();
        let longest = candidates.iter().map(|(length, _)| *length).max()?;
        candidates.retain(|(length, _)| *length == longest);
        candidates.sort_by(|left, right| left.1.cmp(&right.1));
        candidates
            .windows(2)
            .all(|pair| pair[0].1 != pair[1].1)
            .then(|| candidates.first().map(|(_, path)| path.clone()))
            .flatten()
    }

    /// Ask Dune to materialize the project file it reports for `file`, then
    /// return the exact source-mapping root PET must use as its workspace.
    /// This is a real Dune build and has no interactive-operation deadline.
    pub(crate) fn prepare_pet_workspace(&self, file: &Path) -> Result<PathBuf> {
        let workspace = self.pet_workspace_by_file.get(file).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune source has no PET workspace mapping",
            )
        })?;
        prepare_pet_workspace(&self.workspace_root, workspace)
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

    /// Returns inputs named by Dune's selected rules. PET uses their content
    /// fingerprint to invalidate states when the Dune-owned environment moves.
    pub(crate) fn inputs(&self) -> Vec<PathBuf> {
        self.inputs.clone()
    }
}

/// Dune's generated project target is the sole source of PET load-path flags.
/// If a theory did not request one, retain PET's native root behavior rather
/// than synthesizing an `_RocqProject` in the wrapper.
fn prepare_pet_workspace(dune_root: &Path, workspace: &PetWorkspace) -> Result<PathBuf> {
    let Some(target) = &workspace.project_target else {
        return Ok(workspace.root.clone());
    };
    let output = run_dune(
        [OsString::from("build"), target.as_os_str().to_owned()],
        dune_root,
        None,
        None,
    )
    .map_err(|error| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            format!("failed to execute Dune PET workspace target: {error}"),
        )
    })?;
    if output.overflow {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune PET workspace output exceeded the configured bound",
        ));
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            format!(
                "Dune failed to generate the PET workspace: {}",
                stderr.trim()
            ),
        ));
    }
    let project_file = dune_root.join(target);
    if !project_file.is_file() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune PET workspace target is unavailable",
        ));
    }
    Ok(workspace.root.clone())
}

/// Ask Dune which workspace owns an attached path.
/// No dune-project/dune filename search is used as a second workspace model.
pub(crate) fn dune_workspace_root(root: &Path, timeout: Duration) -> Result<PathBuf> {
    let isolated = tempfile::tempdir().map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune metadata directory unavailable",
        )
    })?;
    let build_dir = isolated.path().join("build");
    let output = run_dune(
        [
            OsString::from("describe"),
            OsString::from("workspace"),
            OsString::from("--format"),
            OsString::from("sexp"),
            OsString::from("--lang"),
            OsString::from("0.1"),
        ],
        root,
        Some(timeout),
        Some(&build_dir),
    )
    .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune is unavailable"))?;
    if output.timed_out {
        return Err(Error::new(
            ErrorKind::ProjectTimeout,
            "Dune workspace discovery timed out",
        ));
    }
    if output.overflow || !output.status.success() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune workspace discovery failed",
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune workspace is not UTF-8",
        )
    })?;
    let tokens = sexp_tokens(&text)?;
    let path = tokens
        .windows(2)
        .find_map(|pair| (pair[0] == "root").then(|| PathBuf::from(&pair[1])))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune workspace root unavailable",
            )
        })?;
    fs::canonicalize(path).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune workspace root unavailable",
        )
    })
}

/// Asks Dune for workspace metadata. The runner owns contention and process
/// deadlines; this adapter maps its native outcome to layout error classes.
fn dune_describe(
    workspace: &Path,
    args: &[&str],
    build_dir: &Path,
    timeout: Duration,
) -> Result<NativeOutput> {
    let output = run_dune(
        args.iter().map(OsString::from).collect::<Vec<_>>(),
        workspace,
        Some(timeout),
        Some(build_dir),
    )
    .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune is unavailable"))?;
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

/// Returns the context path reported by Dune for one explicitly isolated build
/// directory. The caller retains that directory while querying its rules.
fn dune_build_context(workspace: &Path, build_dir: &Path, timeout: Duration) -> Result<PathBuf> {
    let output = dune_describe(
        workspace,
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
    let directory = tokens
        .windows(2)
        .find_map(|pair| (pair[0] == "build_context").then(|| workspace.join(&pair[1])))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune build context unavailable",
            )
        })?;
    Ok(directory)
}

/// Ask Dune for the Rocq compilation rules it actually selected. The source
/// and logical prefix come from the same compiler action, so excluded trees,
/// generated targets, and `(modules ...)` need no separate scanner policy.
/// Errors if Dune cannot describe the workspace. This command does not compile
/// or mutate project sources.
// RISK: `dune describe rules` has documented S-expression output but no
// versioned schema. If Dune changes its action form, discovery fails closed.
fn dune_sources(root: &Path, timeout: Duration) -> Result<DuneSourceLayout> {
    let workspace = dune_workspace_root(root, timeout)?;
    let scope = root
        .strip_prefix(&workspace)
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "invalid Dune scope"))?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    // Design note: both queries share one private build tree. Neither reads
    // nor classifies the lock or mutable state of a concurrent project build.
    let isolated = tempfile::tempdir().map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune metadata directory unavailable",
        )
    })?;
    let build_dir = isolated.path().join("build");
    let build_root = dune_build_context(&workspace, &build_dir, timeout)?;
    let mut args = vec!["describe", "rules"];
    if !scope.as_os_str().is_empty() {
        args.push(scope.to_str().ok_or_else(|| {
            Error::new(ErrorKind::InvalidConfiguration, "Dune scope is not UTF-8")
        })?);
    }
    let output = dune_describe(
        &workspace,
        &args,
        &build_dir,
        deadline.saturating_duration_since(Instant::now()),
    )?;
    let text = String::from_utf8(output.stdout)
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Dune rules are not UTF-8"))?;
    let tokens = sexp_tokens(&text)?;
    let mut sources = Vec::new();
    let mut mappings = Vec::new();
    let mut inputs = Vec::new();
    let mut pet_projects = Vec::new();
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
        for pair in rule.windows(2).filter(|pair| pair[0] == "In_source_tree") {
            let input = fs::canonicalize(workspace.join(&pair[1])).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Dune source-tree input is unavailable",
                )
            })?;
            if input.is_file() && !inputs.contains(&input) {
                inputs.push(input);
            }
        }
        // `(generate_project_file)` is represented by Dune as a promoted
        // `_RocqProject` write-file rule. Retain Dune's reported target and
        // mapping directory; the PET wrapper never reconstructs its flags.
        if rule.iter().any(|token| token == "write-file")
            && let Some(target) = rule.iter().find(|token| {
                Path::new(token.as_str())
                    .file_name()
                    .is_some_and(|name| name == "_RocqProject")
            })
        {
            let target = Path::new(target);
            let relative = target.strip_prefix(&build_root).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Dune PET project target is outside its build context",
                )
            })?;
            let directory = relative.parent().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Dune PET project target has no directory",
                )
            })?;
            let source_directory = fs::canonicalize(workspace.join(directory)).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Dune PET workspace directory is unavailable",
                )
            })?;
            let target = relative.to_owned();
            if !pet_projects.contains(&(source_directory.clone(), target.clone())) {
                pet_projects.push((source_directory, target));
            }
        }
        // A theory without modules has no `.vo` rule, but its dependency rule
        // still declares the directory-to-logical-name mapping needed when a
        // new module is created.
        // Design note: with a nondefault DUNE_BUILD_DIR, an isolated query can
        // see existing build artifacts as source-tree files. A copied
        // `.theory.d` is not a theory declaration; only Rocq's dep action is.
        if let Some(target) = rule
            .iter()
            .find(|token| token.ends_with(".theory.d"))
            .filter(|_| {
                rule.windows(2)
                    .any(|pair| pair[0].ends_with("rocq") && pair[1] == "dep")
            })
        {
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
            let target = rule
                .iter()
                .find(|token| token.ends_with(".vo") && Path::new(token).starts_with(&build_root))
                .ok_or_else(|| {
                    Error::new(ErrorKind::InvalidConfiguration, "Rocq rule lacks target")
                })?;
            let build_target = Path::new(target)
                .strip_prefix(&build_root)
                .map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "Rocq target is outside Dune's build context",
                    )
                })?
                .to_owned();
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
                sources.push((source, prefix, build_target));
            }
        }
        start = end;
    }
    inputs.sort();
    Ok((workspace, sources, mappings, inputs, pet_projects))
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
