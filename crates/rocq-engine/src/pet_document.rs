//! Latest-source temporary document construction for disposable PET replay.

use super::*;
use std::path::{Path, PathBuf};

/// Immutable semantic input used to construct an engine-owned PET workspace.
/// Paths are relative to the attached project and never cross the engine API.
#[derive(Clone, Debug)]
pub(crate) struct PetDocumentSpec {
    pub(crate) files: Vec<(PathBuf, Vec<u8>)>,
    pub(crate) target: PathBuf,
}

/// Resolve and validate the latest source view for one PET replay. Existing
/// proof bodies are intentionally discarded; the temporary document ends in
/// `Abort.` so PET can enter proof mode without trusting an admission.
pub(crate) fn pet_document_spec(
    project: &Path,
    state_parent: &Path,
    root: &OpenDeclaration,
) -> Result<PetDocumentSpec> {
    validate_pet_root(root)?;
    let owned_paths = if state_parent != project && state_parent.starts_with(project) {
        vec![state_parent.to_owned()]
    } else {
        Vec::new()
    };
    let layout = layout::Layout::load(project, &owned_paths)?;
    let pending = BTreeMap::new();
    let catalog = discover(project, owned_paths, &pending)?;
    let (target, _) = layout.target(&root.identity.library)?;
    if !target.starts_with(project) {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "PET target is outside project",
        ));
    }
    let target_source = catalog.sources.get(&root.identity);
    let target_content = if let Some(source) = target_source {
        if source.info.kind != root.kind
            || source.info.statement != root.anchor.normalized_statement
            || source.info.context != root.anchor.context
        {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "declaration interface changed",
            ));
        }
        let source_text = read_source(&source.path)?;
        // Reparse after reading so a concurrent edit cannot make the old byte
        // anchor select a different declaration.  Proof bodies may change,
        // but the declaration kind/header/context must still be identical.
        let library = layout.library(&target)?.clone();
        let current = parse_file(&source_text, &library)?
            .into_iter()
            .find(|item| item.info.identity == root.identity)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::DeclarationChanged,
                    "declaration disappeared while preparing PET",
                )
            })?;
        if current.info.kind != root.kind
            || current.info.statement != root.anchor.normalized_statement
            || current.info.context != root.anchor.context
        {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "declaration interface changed",
            ));
        }
        let bytes = source_text.as_bytes();
        let end = current.header_end.min(bytes.len());
        let mut content = bytes[..end].to_vec();
        content.extend_from_slice(b"\nAbort.\n");
        for scope in root.anchor.context.iter().rev() {
            content.extend_from_slice(format!("End {}.\n", scope_name(scope)).as_bytes());
        }
        content
    } else if target.exists() {
        let source = read_source(&target)?;
        insert_synthetic_declaration(&source, root)?
    } else {
        let mut content = root.anchor.normalized_statement.as_bytes().to_vec();
        content.extend_from_slice(b".\nAbort.\n");
        for scope in root.anchor.context.iter().rev() {
            content.extend_from_slice(format!("End {}.\n", scope_name(scope)).as_bytes());
        }
        content
    };
    let target_relative = target
        .strip_prefix(project)
        .map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "PET target is outside project",
            )
        })?
        .to_owned();
    let mut files = mirror_project_inputs(project, state_parent)?;
    files.retain(|(path, _)| path != &target_relative);
    files.push((target_relative.clone(), target_content));
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(PetDocumentSpec {
        files,
        target: target_relative,
    })
}

fn validate_pet_root(root: &OpenDeclaration) -> Result<()> {
    validate_identity(&root.identity)?;
    let modules = root
        .anchor
        .context
        .iter()
        .filter_map(|scope| match scope {
            LexicalScope::Module(name) => Some(name),
            LexicalScope::Section(_) => None,
        })
        .collect::<Vec<_>>();
    if modules != root.identity.modules.iter().collect::<Vec<_>>() {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "PET declaration context does not match its identity",
        ));
    }
    if root.anchor.context.len() > 128
        || root
            .anchor
            .context
            .iter()
            .any(|scope| !valid_identifier(scope_name(scope)))
    {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "PET declaration context is invalid",
        ));
    }
    if root.anchor.normalized_statement.is_empty()
        || root.anchor.normalized_statement.len() > 1024 * 1024
    {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "PET declaration header is empty or oversized",
        ));
    }
    let Some((kind, name, has_term_body)) = declaration_header(&root.anchor.normalized_statement)
    else {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "PET declaration header is invalid",
        ));
    };
    if kind != root.kind || name != root.identity.constant {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "PET declaration header identity does not match",
        ));
    }
    // A direct term declaration is already closed and therefore cannot be a
    // replay root; theorem proofs must enter the interactive form.
    if has_term_body {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "PET replay root is already a term declaration",
        ));
    }
    Ok(())
}

/// Insert a synthetic declaration into an existing source document without
/// changing any source outside the temporary PET workspace.  The declaration
/// must be placed while the requested lexical context is live; inserting it
/// before that context's closing `End` preserves all surrounding modules and
/// sections.  If the context is left open at EOF, the temporary document
/// closes it after the synthetic proof so PET sees a complete source.
fn insert_synthetic_declaration(source: &str, root: &OpenDeclaration) -> Result<Vec<u8>> {
    let wanted = &root.anchor.context;
    let mut scopes = Vec::<LexicalScope>::new();
    let mut ignored_scopes = Vec::<String>::new();
    let mut insertion = None;

    for range in sentence_ranges(source)? {
        let normalized = normalize_sentence(&source[range.clone()]);
        let Some(event) = scope_event(&normalized) else {
            continue;
        };
        match event {
            ScopeEvent::Open(scope) => scopes.push(scope),
            ScopeEvent::Ignored(name) => ignored_scopes.push(name),
            ScopeEvent::Close(name) => {
                if ignored_scopes
                    .last()
                    .is_some_and(|last| name.is_empty() || *last == name)
                {
                    ignored_scopes.pop();
                    continue;
                }

                if let Some(last) = scopes.last()
                    && scopes == *wanted
                    && (name.is_empty() || scope_name(last) == name)
                {
                    if insertion.is_some() {
                        return Err(Error::new(
                            ErrorKind::Ambiguous,
                            "synthetic declaration context is ambiguous",
                        ));
                    }
                    insertion = Some(range.start);
                }

                if let Some(last) = scopes.last() {
                    if name.is_empty() || scope_name(last) == name {
                        scopes.pop();
                    } else {
                        return Err(Error::new(
                            ErrorKind::InvalidConfiguration,
                            "lexical scope closes out of order",
                        ));
                    }
                } else if !name.is_empty() {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "lexical scope closes out of order",
                    ));
                }
            }
        }
    }

    let mut content = source.as_bytes().to_vec();
    let insertion = match insertion {
        Some(position) => position,
        None => {
            if !ignored_scopes.is_empty() || scopes != *wanted {
                return Err(Error::new(
                    ErrorKind::InvalidDeclaration,
                    "synthetic declaration context is not live",
                ));
            }
            source.len()
        }
    };

    let mut declaration = root.anchor.normalized_statement.as_bytes().to_vec();
    if insertion == source.len() && !source.is_empty() && !source.ends_with('\n') {
        declaration.insert(0, b'\n');
    }
    declaration.extend_from_slice(b".\nAbort.\n");
    if insertion == source.len() {
        for scope in wanted.iter().rev() {
            declaration.extend_from_slice(format!("End {}.\n", scope_name(scope)).as_bytes());
        }
    }
    content.splice(insertion..insertion, declaration);
    Ok(content)
}

fn mirror_project_inputs(project: &Path, state_parent: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    let mut total = 0usize;
    for entry in walkdir::WalkDir::new(project).follow_links(false) {
        let entry = entry
            .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "project traversal failed"))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(project)
            .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "project path invalid"))?;
        if relative.as_os_str().is_empty() || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::Normal(value)
                    if matches!(value.to_str(), Some("_build" | ".git" | ".hg" | ".svn" | "target"))
            )
        }) || (state_parent != project && path.starts_with(state_parent))
            || !entry.file_type().is_file()
        {
            continue;
        }
        let metadata = entry.metadata().map_err(|_| {
            Error::new(ErrorKind::InvalidConfiguration, "project input unavailable")
        })?;
        if metadata.len() > 16 * 1024 * 1024 {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "project input file is oversized",
            ));
        }
        let bytes = std::fs::read(path).map_err(|_| {
            Error::new(ErrorKind::InvalidConfiguration, "project input unavailable")
        })?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "project input file is oversized",
            ));
        }
        total = total.checked_add(bytes.len()).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "project inputs are oversized",
            )
        })?;
        if total > 64 * 1024 * 1024 {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "project inputs are oversized",
            ));
        }
        files.push((relative.to_owned(), bytes));
    }
    Ok(files)
}

pub(crate) fn read_source(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path)
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "source unavailable"))?;
    if metadata.len() > MAX_SOURCE_BYTES as u64 {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "source file is oversized",
        ));
    }
    let source = std::fs::read_to_string(path)
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "source unavailable"))?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "source file is oversized",
        ));
    }
    Ok(source)
}
