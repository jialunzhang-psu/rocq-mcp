//! Declaration/source indexing over safe sentence spans.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceDeclaration {
    pub(crate) info: DeclarationInfo,
    pub(crate) path: PathBuf,
    pub(crate) range: Range<usize>,
    pub(crate) header_end: usize,
    pub(crate) anchor: SourceAnchor,
}

#[derive(Clone, Debug)]
struct Sentence {
    range: Range<usize>,
    normalized: String,
}

#[derive(Clone, Debug)]
struct PendingDeclaration {
    info: DeclarationInfo,
    start: usize,
    header_end: usize,
}

/// Transient byte positions within one source snapshot, never a declaration identity.
struct DeclarationSpan {
    range: Range<usize>,
    header_end: usize,
    body_start: usize,
}

pub(crate) fn discover(
    root: &Path,
    owned_paths: Vec<PathBuf>,
    pending: &BTreeMap<DeclarationIdentity, PendingRecord>,
) -> Result<ProjectCatalog> {
    let layout = layout::Layout::load(root, &owned_paths)?;
    let mut declarations = Vec::new();
    let mut sources = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for file in layout.files() {
        let lib = layout.library(&file)?.clone();
        let source = read_source(&file)?;
        let parsed = parse_file(&source, &lib)?;
        for item in parsed {
            if !seen.insert(item.info.identity.clone()) {
                return Err(Error::new(
                    ErrorKind::Ambiguous,
                    "declaration identity is ambiguous",
                ));
            }
            sources.insert(
                item.info.identity.clone(),
                SourceDeclaration {
                    info: item.info.clone(),
                    path: file.clone(),
                    range: item.range,
                    header_end: item.header_end,
                    anchor: item.anchor,
                },
            );
            declarations.push(item.info);
        }
    }

    // A durable candidate may outlive its source declaration (for example, a
    // new logical library).  Keep its semantic identity visible in catalog.
    for (identity, record) in pending {
        if seen.contains(identity) {
            continue;
        }
        let candidate = match &record.phase {
            ProofPhase::Solved(candidate) => &candidate.declaration,
            ProofPhase::Closed(closed) => &closed.candidate.declaration,
            ProofPhase::Rejected(rejected) => &rejected.candidate().declaration,
        };
        let info = DeclarationInfo {
            identity: identity.clone(),
            context: candidate.anchor.context.clone(),
            kind: candidate.kind,
            statement: candidate.anchor.normalized_statement.clone(),
            status: ProofLifecycle::Pending,
        };
        declarations.push(info);
    }

    // Recovery overrides source status but never mutates the parsed source
    // identity or anchor.
    for info in &mut declarations {
        if let Some(record) = pending.get(&info.identity) {
            info.status = match &record.phase {
                ProofPhase::Rejected(_) => ProofLifecycle::Rejected,
                ProofPhase::Solved(_) => ProofLifecycle::Pending,
                // A closed record is recoverable publication acknowledgement;
                // source is already the authoritative completed view.
                ProofPhase::Closed(_) => ProofLifecycle::Completed,
            };
        }
    }
    Ok(ProjectCatalog {
        root: root.to_owned(),
        declarations,
        sources,
    })
}

pub(crate) fn parse_file(source: &str, lib: &LogicalLibrary) -> Result<Vec<SourceDeclaration>> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "source file is oversized",
        ));
    }
    let sentences = sentence_ranges(source)?
        .into_iter()
        .map(|range| {
            let normalized = normalize_sentence(&source[range.clone()]);
            Sentence { range, normalized }
        })
        .collect::<Vec<_>>();
    let source_digest: [u8; 32] = Sha256::digest(source.as_bytes()).into();
    let mut output = Vec::new();
    let mut scopes = Vec::<LexicalScope>::new();
    let mut ignored_scopes = Vec::<String>::new();
    let mut pending: Option<PendingDeclaration> = None;

    for sentence in &sentences {
        if let Some(current) = pending.take() {
            if let Some(terminator) = proof_terminator(&sentence.normalized) {
                let end = sentence.range.end;
                output.push(finish_declaration(
                    source,
                    lib,
                    current,
                    end,
                    terminator,
                    source_digest,
                ));
                continue;
            }
            if starts_declaration(&sentence.normalized)
                || scope_event(&sentence.normalized).is_some()
            {
                output.push(finish_open_declaration(
                    source,
                    lib,
                    current,
                    sentence.range.start,
                    source_digest,
                ));
                // process this sentence structurally below
            } else {
                pending = Some(current);
                continue;
            }
        }

        if ignored_scopes.is_empty()
            && let Some((kind, name, has_term_body)) = declaration_header(&sentence.normalized)
        {
            let modules = scopes
                .iter()
                .filter_map(|scope| match scope {
                    LexicalScope::Module(name) => Some(name.clone()),
                    LexicalScope::Section(_) => None,
                })
                .collect::<Vec<_>>();
            let identity = DeclarationIdentity {
                library: lib.clone(),
                modules,
                constant: name,
            };
            let info = DeclarationInfo {
                identity,
                context: scopes.clone(),
                kind,
                statement: sentence.normalized.clone(),
                status: if has_term_body {
                    ProofLifecycle::Completed
                } else {
                    ProofLifecycle::Open
                },
            };
            if has_term_body {
                let body_start = assignment_body_start(source, &sentence.range);
                output.push(make_source_declaration_with_status(
                    source,
                    info,
                    DeclarationSpan {
                        range: sentence.range.clone(),
                        header_end: sentence.range.end,
                        body_start,
                    },
                    ProofLifecycle::Completed,
                    source_digest,
                ));
            } else {
                pending = Some(PendingDeclaration {
                    info,
                    start: sentence.range.start,
                    header_end: sentence.range.end,
                });
            }
            continue;
        }

        if let Some(scope) = scope_event(&sentence.normalized) {
            match scope {
                ScopeEvent::Open(scope) => scopes.push(scope),
                ScopeEvent::Ignored(name) => ignored_scopes.push(name),
                ScopeEvent::Close(name) => {
                    if ignored_scopes
                        .last()
                        .is_some_and(|last| name.is_empty() || *last == name)
                    {
                        ignored_scopes.pop();
                    } else if let Some(last) = scopes.last() {
                        if scope_name(last) == name || name.is_empty() {
                            scopes.pop();
                        } else {
                            return Err(Error::new(
                                ErrorKind::InvalidConfiguration,
                                "lexical scope closes out of order",
                            ));
                        }
                    }
                }
            }
        }
    }
    if let Some(current) = pending {
        output.push(finish_open_declaration(
            source,
            lib,
            current,
            source.len(),
            source_digest,
        ));
    }
    Ok(output)
}

fn finish_declaration(
    source: &str,
    _lib: &LogicalLibrary,
    current: PendingDeclaration,
    end: usize,
    terminator: ProofTerminatorKind,
    source_digest: [u8; 32],
) -> SourceDeclaration {
    let status = match terminator {
        ProofTerminatorKind::Qed if current.info.kind != DeclarationKind::Definition => {
            ProofLifecycle::Completed
        }
        ProofTerminatorKind::Defined if current.info.kind == DeclarationKind::Definition => {
            ProofLifecycle::Completed
        }
        ProofTerminatorKind::Qed | ProofTerminatorKind::Defined => ProofLifecycle::Open,
        ProofTerminatorKind::Admitted | ProofTerminatorKind::Abort => ProofLifecycle::Open,
    };
    make_source_declaration_with_status(
        source,
        current.info,
        DeclarationSpan {
            range: current.start..end,
            header_end: current.header_end,
            body_start: current.header_end,
        },
        status,
        source_digest,
    )
}

fn finish_open_declaration(
    source: &str,
    _lib: &LogicalLibrary,
    current: PendingDeclaration,
    end: usize,
    source_digest: [u8; 32],
) -> SourceDeclaration {
    make_source_declaration_with_status(
        source,
        current.info,
        DeclarationSpan {
            range: current.start..end,
            header_end: current.header_end,
            body_start: current.header_end,
        },
        ProofLifecycle::Open,
        source_digest,
    )
}

fn assignment_body_start(source: &str, sentence: &Range<usize>) -> usize {
    let text = &source[sentence.clone()];
    let chars = text.char_indices().collect::<Vec<_>>();
    let mut comment_depth = 0usize;
    let mut string = false;
    let mut index = 0usize;
    while index < chars.len() {
        let (offset, c) = chars[index];
        let next = chars.get(index + 1).copied();
        if comment_depth > 0 {
            if c == '(' && next.is_some_and(|(_, value)| value == '*') {
                comment_depth += 1;
                index += 2;
                continue;
            }
            if c == '*' && next.is_some_and(|(_, value)| value == ')') {
                comment_depth -= 1;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if string {
            if c == '"' {
                if next.is_some_and(|(_, value)| value == '"') {
                    index += 2;
                    continue;
                }
                string = false;
            }
            index += 1;
            continue;
        }
        if c == '(' && next.is_some_and(|(_, value)| value == '*') {
            comment_depth = 1;
            index += 2;
            continue;
        }
        if c == '"' {
            string = true;
            index += 1;
            continue;
        }
        if c == ':' && next.is_some_and(|(_, value)| value == '=') {
            return sentence.start + offset + 2;
        }
        index += 1;
    }
    sentence.end
}

fn make_source_declaration_with_status(
    source: &str,
    mut info: DeclarationInfo,
    span: DeclarationSpan,
    status: ProofLifecycle,
    source_digest: [u8; 32],
) -> SourceDeclaration {
    info.status = status;
    let body_end = span
        .range
        .end
        .min(source.len())
        .max(span.body_start.min(source.len()));
    let body_start = span.body_start.min(body_end);
    let old_body_digest: [u8; 32] = Sha256::digest(&source.as_bytes()[body_start..body_end]).into();
    SourceDeclaration {
        anchor: SourceAnchor {
            source_digest,
            normalized_statement: info.statement.clone(),
            context: info.context.clone(),
            old_body_digest,
        },
        info,
        path: PathBuf::new(),
        range: span.range.start..span.range.end.min(source.len()),
        header_end: span.header_end.min(source.len()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProofTerminatorKind {
    Qed,
    Defined,
    Admitted,
    Abort,
}

fn proof_terminator(sentence: &str) -> Option<ProofTerminatorKind> {
    let words = lexical_words(sentence);
    let words = if words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("time"))
    {
        words.get(1..).unwrap_or_default()
    } else {
        words.as_slice()
    };
    if words.len() != 1 {
        return None;
    }
    match words[0].to_ascii_lowercase().as_str() {
        "qed" => Some(ProofTerminatorKind::Qed),
        "defined" => Some(ProofTerminatorKind::Defined),
        "admitted" => Some(ProofTerminatorKind::Admitted),
        "abort" => Some(ProofTerminatorKind::Abort),
        _ => None,
    }
}

pub(crate) fn declaration_header(sentence: &str) -> Option<(DeclarationKind, String, bool)> {
    let tokens = lexical_words(sentence);
    let mut index = 0;
    while index < tokens.len() {
        let lower = tokens[index].to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "local" | "global" | "polymorphic" | "monomorphic"
        ) {
            index += 1;
            continue;
        }
        let kind = match lower.as_str() {
            "theorem" => DeclarationKind::Theorem,
            "lemma" => DeclarationKind::Lemma,
            "fact" => DeclarationKind::Fact,
            "remark" => DeclarationKind::Remark,
            "corollary" => DeclarationKind::Corollary,
            "proposition" => DeclarationKind::Proposition,
            "definition" => DeclarationKind::Definition,
            _ => return None,
        };
        let name = tokens.get(index + 1)?.clone();
        if !valid_identifier(&name) {
            return None;
        }
        let has_term_body =
            kind == DeclarationKind::Definition && sentence_contains_assignment(sentence, &name);
        return Some((kind, name, has_term_body));
    }
    None
}

fn sentence_contains_assignment(sentence: &str, name: &str) -> bool {
    let Some(position) = sentence.find(name) else {
        return false;
    };
    sentence[position + name.len()..].contains(":=")
}

fn starts_declaration(sentence: &str) -> bool {
    declaration_header(sentence).is_some()
}

pub(crate) enum ScopeEvent {
    Open(LexicalScope),
    Ignored(String),
    Close(String),
}

pub(crate) fn scope_event(sentence: &str) -> Option<ScopeEvent> {
    let words = lexical_words(sentence);
    let first = words.first()?.to_ascii_lowercase();
    match first.as_str() {
        "section" => words
            .get(1)
            .map(|name| ScopeEvent::Open(LexicalScope::Section(name.clone()))),
        "module" => {
            let second = words.get(1)?.to_ascii_lowercase();
            if second == "type" {
                return words.get(2).cloned().map(ScopeEvent::Ignored);
            }
            if matches!(second.as_str(), "import" | "export") {
                return None;
            }
            // `Module X := Y.` and `Module X (…):=…` are aliases, not scopes.
            if sentence.contains(":=") {
                return None;
            }
            Some(ScopeEvent::Open(LexicalScope::Module(words[1].clone())))
        }
        "end" => Some(ScopeEvent::Close(words.get(1).cloned().unwrap_or_default())),
        _ => None,
    }
}

pub(crate) fn scope_name(scope: &LexicalScope) -> &str {
    match scope {
        LexicalScope::Module(name) | LexicalScope::Section(name) => name,
    }
}

/// Finds a lexical insertion context without relying on line layout.  An empty
/// context is always valid; a non-empty context must be live at some sentence
/// boundary, including sections containing no declarations.
pub(crate) fn context_occurrences(source: &str, wanted: &[LexicalScope]) -> Result<usize> {
    if wanted.is_empty() {
        return Ok(1);
    }
    let mut scopes = Vec::new();
    let mut ignored_scopes: Vec<String> = Vec::new();
    let mut occurrences = 0usize;
    for range in sentence_ranges(source)? {
        let normalized = normalize_sentence(&source[range]);
        if let Some(event) = scope_event(&normalized) {
            match event {
                ScopeEvent::Open(scope) => {
                    scopes.push(scope);
                    if scopes == wanted {
                        occurrences = occurrences.saturating_add(1);
                    }
                }
                ScopeEvent::Ignored(name) => ignored_scopes.push(name),
                ScopeEvent::Close(name) => {
                    if ignored_scopes
                        .last()
                        .is_some_and(|last| name.is_empty() || *last == name)
                    {
                        ignored_scopes.pop();
                    } else if scopes
                        .last()
                        .is_some_and(|last| name.is_empty() || scope_name(last) == name)
                    {
                        scopes.pop();
                    }
                }
            }
        }
    }
    Ok(occurrences)
}

/// Returns the byte offset immediately before the closing sentence of one
/// unique lexical context.  New declarations use this boundary instead of
/// appending a second Module/Section wrapper at end of file.
pub(crate) fn context_insertion_offset(
    source: &str,
    wanted: &[LexicalScope],
) -> Result<Option<usize>> {
    if wanted.is_empty() {
        return Ok(Some(source.len()));
    }
    let mut scopes = Vec::new();
    for range in sentence_ranges(source)? {
        let normalized = normalize_sentence(&source[range.clone()]);
        if let Some(event) = scope_event(&normalized) {
            match event {
                ScopeEvent::Open(scope) => scopes.push(scope),
                ScopeEvent::Ignored(_) => {}
                ScopeEvent::Close(name) => {
                    if scopes == wanted {
                        return Ok(Some(range.start));
                    }
                    if scopes
                        .last()
                        .is_some_and(|last| name.is_empty() || scope_name(last) == name)
                    {
                        scopes.pop();
                    }
                }
            }
        }
    }
    Ok(None)
}

pub(crate) fn resolve_declaration<'a>(
    catalog: &'a ProjectCatalog,
    name: &str,
) -> Result<&'a DeclarationInfo> {
    validate_name(name)?;
    let matches = catalog
        .declarations
        .iter()
        .filter(|item| declaration_name_matches(&item.identity, name))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [item] => Ok(item),
        [] => Err(Error::new(
            ErrorKind::NotFound,
            format!("declaration '{name}' was not found"),
        )),
        _ => Err(Error::new(
            ErrorKind::Ambiguous,
            "declaration name is ambiguous",
        )),
    }
}

pub(crate) fn declaration_name_matches(identity: &DeclarationIdentity, name: &str) -> bool {
    let parts = name.split('.').collect::<Vec<_>>();
    // Design note: the catalog exposes the library-qualified name, so the
    // same value must be accepted back as a target. Shorter module/constant
    // suffixes remain valid and can still become ambiguous.
    let full = identity
        .library
        .0
        .iter()
        .chain(identity.modules.iter())
        .chain(std::iter::once(&identity.constant))
        .map(String::as_str)
        .collect::<Vec<_>>();
    parts.len() <= full.len() && full[full.len() - parts.len()..].iter().copied().eq(parts)
}

pub(crate) fn format_name(identity: &DeclarationIdentity) -> String {
    identity
        .library
        .0
        .iter()
        .chain(identity.modules.iter())
        .chain(std::iter::once(&identity.constant))
        .cloned()
        .collect::<Vec<_>>()
        .join(".")
}

pub(crate) fn local_name(identity: &DeclarationIdentity) -> String {
    identity
        .modules
        .iter()
        .chain(std::iter::once(&identity.constant))
        .cloned()
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_target_accepts_every_logical_suffix() {
        let identity = DeclarationIdentity {
            library: LogicalLibrary(vec!["Demo".into(), "Main".into()]),
            modules: vec!["Nested".into()],
            constant: "answer".into(),
        };
        for target in [
            "answer",
            "Nested.answer",
            "Main.Nested.answer",
            "Demo.Main.Nested.answer",
        ] {
            assert!(declaration_name_matches(&identity, target), "{target}");
        }
        for target in ["Other.answer", "Demo.answer", "X.Demo.Main.Nested.answer"] {
            assert!(!declaration_name_matches(&identity, target), "{target}");
        }
    }
}
