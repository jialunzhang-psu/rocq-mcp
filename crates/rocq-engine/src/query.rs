//! Typed read-only query dispatch.

use super::*;

impl Engine {
    pub fn query(
        &self,
        project: &Path,
        context: Option<AttemptId>,
        query: Query,
    ) -> Result<QueryResult> {
        self.query_inner(project, context, query)
    }

    fn query_inner(
        &self,
        project: &Path,
        context: Option<AttemptId>,
        query: Query,
    ) -> Result<QueryResult> {
        if matches!(query, Query::Goals) {
            let attempt = context
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "goals requires attempt"))?;
            let actual = self.attempt_project(attempt)?;
            if std::fs::canonicalize(project).ok().as_deref()
                != std::fs::canonicalize(actual).ok().as_deref()
            {
                return Err(Error::new(
                    ErrorKind::DeclarationChanged,
                    "query project does not match attempt",
                ));
            }
            return Ok(QueryResult::State(Box::new(self.inspect(attempt)?)));
        }
        let catalog = self.catalog(project)?;
        let attachment = self.attach_project(project)?;
        let _gate = attachment.read();
        match query {
            Query::Search {
                name,
                statement,
                status,
                offset,
                limit,
            } => {
                validate_search(name.as_deref(), statement.as_deref(), limit)?;
                let mut rows = catalog
                    .declarations
                    .iter()
                    .filter(|i| {
                        name.as_deref()
                            .is_none_or(|value| format_name(&i.identity).contains(value))
                    })
                    .filter(|i| statement.as_deref().is_none_or(|v| i.statement.contains(v)))
                    .filter(|i| status.is_none_or(|v| i.status == v))
                    .map(|i| format_name(&i.identity))
                    .collect::<Vec<_>>();
                let b = offset.min(rows.len());
                let e = b.saturating_add(limit).min(rows.len());
                rows = rows.split_off(b);
                rows.truncate(e - b);
                Ok(QueryResult::Text(rows.join("\n")))
            }
            Query::Statement { name } => Ok(QueryResult::Text(
                resolve_declaration(&catalog, &name)?.statement.clone(),
            )),
            Query::Proof { name } | Query::Definition { name } => {
                let item = resolve_declaration(&catalog, &name)?;
                let src = catalog.sources.get(&item.identity).ok_or_else(|| {
                    Error::new(
                        ErrorKind::NotFound,
                        format!("source for declaration '{name}' was not found"),
                    )
                })?;
                let contents = std::fs::read_to_string(&src.path).map_err(|_| {
                    Error::new(ErrorKind::InvalidConfiguration, "source unavailable")
                })?;
                Ok(QueryResult::Text(contents[src.range.clone()].to_owned()))
            }
            Query::Assumptions { name } => {
                let item = resolve_declaration(&catalog, &name)?;
                self.query_text(
                    project,
                    &catalog,
                    Some(&item.identity.library),
                    pet_runtime::FixedPetQuery::Assumptions(local_name(&item.identity)),
                )
            }
            Query::Dependencies { name } => {
                let item = resolve_declaration(&catalog, &name)?;
                self.query_text(
                    project,
                    &catalog,
                    Some(&item.identity.library),
                    pet_runtime::FixedPetQuery::Dependencies(local_name(&item.identity)),
                )
            }
            Query::ExpressionType { expression } => {
                validate_native_fragment(&expression)?;
                self.query_text(
                    project,
                    &catalog,
                    None,
                    pet_runtime::FixedPetQuery::ExpressionType(expression),
                )
            }
            Query::Notation { expression } => {
                validate_native_fragment(&expression)?;
                self.query_text(
                    project,
                    &catalog,
                    None,
                    pet_runtime::FixedPetQuery::Notation(expression),
                )
            }
            Query::Goals => Err(Error::new(
                ErrorKind::InvalidRequest,
                "goals query requires an open proof",
            )),
        }
    }

    fn query_state(
        &self,
        project: &Path,
        catalog: &ProjectCatalog,
        target_library: Option<&LogicalLibrary>,
    ) -> Result<pet_runtime::PetState> {
        // Design note: a named theorem is only visible after its own library's
        // source prefix. An arbitrary first library cannot answer its audit.
        let library = target_library
            .cloned()
            .or_else(|| {
                catalog
                    .declarations
                    .first()
                    .map(|item| item.identity.library.clone())
            })
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "project has no query environment",
                )
            })?;
        let mut suffix = 0usize;
        let constant =
            loop {
                let candidate = format!("__rocq_engine_query_{suffix}");
                if !catalog.declarations.iter().any(|item| {
                    item.identity.library == library && item.identity.constant == candidate
                }) {
                    break candidate;
                }
                suffix += 1;
            };
        let root = OpenDeclaration {
            kind: DeclarationKind::Theorem,
            identity: DeclarationIdentity {
                library,
                modules: Vec::new(),
                constant: constant.clone(),
            },
            anchor: SourceAnchor {
                source_digest: [0; 32],
                normalized_statement: format!("Theorem {constant} : True"),
                context: Vec::new(),
                old_body_digest: [0; 32],
            },
        };
        self.replay_state(project, &root, &[])
    }

    fn query_text(
        &self,
        project: &Path,
        catalog: &ProjectCatalog,
        target_library: Option<&LogicalLibrary>,
        query: pet_runtime::FixedPetQuery,
    ) -> Result<QueryResult> {
        let deadline = std::time::Instant::now()
            .checked_add(self.config.operation_timeout)
            .unwrap_or_else(std::time::Instant::now);
        let mut protocol_restarted = false;
        loop {
            let state = self.query_state(project, catalog, target_library)?;
            match self.pet_runtime.run_fixed(&state, query.clone()) {
                Ok(text) => {
                    return Ok(QueryResult::Text(text));
                }
                Err(
                    pet_runtime::PetError::Stale
                    | pet_runtime::PetError::Protocol(_)
                    | pet_runtime::PetError::OutputOverflow,
                ) if !protocol_restarted => {
                    protocol_restarted = true;
                    self.pet_runtime.detach(project);
                }
                Err(pet_runtime::PetError::ProcessFailure(_)) => {
                    self.pet_runtime.detach(project);
                    if std::time::Instant::now() >= deadline {
                        return Err(Error::new(ErrorKind::ProofTimeout, "PET query timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(self.pet_error(error)),
            }
        }
    }
}
