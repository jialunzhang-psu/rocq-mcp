//! Interactive trace/PET orchestration.

use super::*;

impl Engine {
    pub fn step(&self, attempt: AttemptId, commands: &str) -> Result<StepResult> {
        self.step_inner(attempt, commands)
    }

    fn step_inner(&self, attempt: AttemptId, commands: &str) -> Result<StepResult> {
        let project = self.attempt_project(attempt)?;
        let attachment = self.attach_project(&project)?;
        let gate = attachment.read();
        let view = self.forest.inspect(attempt.0).map_err(trace_error)?;
        let mut cursor = attempt.0;
        let mut last_state = self.replay_state(&project, view.root(), view.actions())?;
        let ranges = sentence_ranges(commands)
            .map_err(|_| Error::new(ErrorKind::InvalidRequest, "malformed tactic input"))?;
        if ranges.is_empty() {
            return Err(Error::new(ErrorKind::InvalidRequest, "empty tactic input"));
        }
        let mut failure = None;
        for range in ranges {
            let tactic = canonical_tactic(&commands[range])?;
            if tactic.0.is_empty() {
                continue;
            }
            let key = ActionKey::new(tactic.0.as_bytes())
                .map_err(|_| Error::new(ErrorKind::InvalidRequest, "tactic is too large"))?;
            let root = view.root().clone();
            let parent_view = self.forest.inspect(cursor).map_err(trace_error)?;
            let mut actions = parent_view.actions().to_vec();
            actions.push(tactic.clone());
            let project_clone = project.clone();
            match self.forest.step(cursor, key, || {
                self.pet_runtime
                    .replay(&project_clone, &root, &actions)
                    .map(|_| tactic.clone())
            }) {
                Ok(next) => {
                    cursor = next;
                    last_state = self.replay_state(&project, &root, &actions)?;
                }
                Err(trace_forest::CallError::Callback(error)) => {
                    failure = Some(self.pet_error(error));
                    break;
                }
                Err(trace_forest::CallError::Forest(error)) => {
                    failure = Some(trace_error(error));
                    break;
                }
            }
        }
        self.remember_attempt(cursor, &project);
        let mut state = self.state_from_pet(
            AttemptId(cursor),
            view.root(),
            &last_state,
            ProofLifecycle::Open,
            self.forest
                .inspect(cursor)
                .map_err(trace_error)?
                .actions()
                .len(),
        );
        drop(gate);
        if failure.is_none() {
            match self.close_solved(AttemptId(cursor), &last_state) {
                Ok(Some((published, close_error))) => {
                    state = published;
                    failure = close_error;
                }
                Ok(None) => {}
                Err(error) => failure = Some(error),
            }
        }
        let failure = failure;
        Ok(StepResult {
            state,
            error: failure,
        })
    }

    /// Replays each one-sentence extension independently; no candidate edge is appended.
    pub fn candidates(
        &self,
        attempt: AttemptId,
        candidates: &[String],
    ) -> Result<Vec<CandidateResult>> {
        self.candidates_inner(attempt, candidates)
    }

    fn candidates_inner(
        &self,
        attempt: AttemptId,
        candidates: &[String],
    ) -> Result<Vec<CandidateResult>> {
        if !(1..=20).contains(&candidates.len()) {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "candidate count is outside range",
            ));
        }
        let project = self.attempt_project(attempt)?;
        let attachment = self.attach_project(&project)?;
        let _gate = attachment.read();
        let view = self.forest.inspect(attempt.0).map_err(trace_error)?;
        let mut out = Vec::with_capacity(candidates.len());
        for text in candidates {
            let ranges = sentence_ranges(text)
                .map_err(|_| Error::new(ErrorKind::InvalidRequest, "malformed candidate"))?;
            if ranges.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "candidate must be one sentence",
                ));
            }
            let tactic = canonical_tactic(&text[ranges[0].clone()])?;
            // Candidate actions are not committed, but they must obey the same
            // bounded canonical representation as committed trace edges.
            ActionKey::new(tactic.0.as_bytes())
                .map_err(|_| Error::new(ErrorKind::InvalidRequest, "candidate is too large"))?;
            let mut actions = view.actions().to_vec();
            actions.push(tactic);
            match self.pet_runtime.replay(&project, view.root(), &actions) {
                Ok(native) => out.push(CandidateResult {
                    solved: native.proof_finished && native.goals.all_clear(),
                    state: Some(self.state_from_pet(
                        attempt,
                        view.root(),
                        &native,
                        ProofLifecycle::Open,
                        actions.len(),
                    )),
                    error: None,
                }),
                Err(error) => out.push(CandidateResult {
                    solved: false,
                    state: None,
                    error: Some(self.pet_error(error)),
                }),
            }
        }
        Ok(out)
    }

    /// Replays the selected immutable prefix and returns PET's structured goals.
    pub fn inspect(&self, attempt: AttemptId) -> Result<ProofState> {
        self.inspect_inner(attempt)
    }

    fn inspect_inner(&self, attempt: AttemptId) -> Result<ProofState> {
        let project = self.attempt_project(attempt)?;
        let attachment = self.attach_project(&project)?;
        let _gate = attachment.read();
        let view = self.forest.inspect(attempt.0).map_err(trace_error)?;
        let native = self.replay_state(&project, view.root(), view.actions())?;
        Ok(self.state_from_pet(
            attempt,
            view.root(),
            &native,
            ProofLifecycle::Open,
            view.actions().len(),
        ))
    }
}
