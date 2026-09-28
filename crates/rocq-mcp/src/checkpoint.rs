//! MCP-owned request-boundary checkpoint graph.
//!
//! One node represents one successful `check` request, regardless of how many
//! Rocq sentences the accepted PET fragment contains. PET state IDs are merely
//! optional handles stored on nodes; all topology and replay text lives here.

use rocq_engine::{DeclarationTarget, PetStateId, ProofState};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CheckpointId(u64);

impl CheckpointId {
    pub(crate) fn from_u64(value: u64) -> Self {
        Self(value)
    }
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Checkpoint {
    pub(crate) parent: Option<CheckpointId>,
    pub(crate) pet_state: Option<PetStateId>,
    pub(crate) accepted_input: Option<String>,
    pub(crate) finished: bool,
    pub(crate) view: ProofState,
}

#[derive(Clone, Debug)]
pub(crate) struct ProofSession {
    pub(crate) target: DeclarationTarget,
    pub(crate) current: CheckpointId,
    pub(crate) checkpoints: BTreeMap<CheckpointId, Checkpoint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckpointError {
    NoActiveProof,
    ActiveProof,
    Unknown,
    TooFar,
    Exhausted,
}

#[derive(Default)]
pub(crate) struct CheckpointBook {
    next: u64,
    pub(crate) proof: Option<ProofSession>,
}

impl CheckpointBook {
    pub(crate) fn begin(
        &mut self,
        target: DeclarationTarget,
        state: PetStateId,
        finished: bool,
        view: ProofState,
    ) -> Result<CheckpointId, CheckpointError> {
        if self.proof.is_some() {
            return Err(CheckpointError::ActiveProof);
        }
        let id = self.allocate()?;
        let root = Checkpoint {
            parent: None,
            pet_state: Some(state),
            accepted_input: None,
            finished,
            view,
        };
        let mut checkpoints = BTreeMap::new();
        checkpoints.insert(id, root);
        self.proof = Some(ProofSession {
            target,
            current: id,
            checkpoints,
        });
        Ok(id)
    }

    pub(crate) fn commit(
        &mut self,
        state: PetStateId,
        input: String,
        finished: bool,
        view: ProofState,
    ) -> Result<CheckpointId, CheckpointError> {
        let id = self.allocate()?;
        let proof = self.proof.as_mut().ok_or(CheckpointError::NoActiveProof)?;
        let parent = proof.current;
        proof.checkpoints.insert(
            id,
            Checkpoint {
                parent: Some(parent),
                pet_state: Some(state),
                accepted_input: Some(input),
                finished,
                view,
            },
        );
        proof.current = id;
        Ok(id)
    }

    pub(crate) fn current(&self) -> Result<(CheckpointId, &Checkpoint), CheckpointError> {
        let proof = self.proof.as_ref().ok_or(CheckpointError::NoActiveProof)?;
        let checkpoint = proof
            .checkpoints
            .get(&proof.current)
            .ok_or(CheckpointError::Unknown)?;
        Ok((proof.current, checkpoint))
    }

    pub(crate) fn lookup(&self, id: CheckpointId) -> Result<&Checkpoint, CheckpointError> {
        self.proof
            .as_ref()
            .ok_or(CheckpointError::NoActiveProof)?
            .checkpoints
            .get(&id)
            .ok_or(CheckpointError::Unknown)
    }

    pub(crate) fn select(&mut self, id: CheckpointId) -> Result<(), CheckpointError> {
        let proof = self.proof.as_mut().ok_or(CheckpointError::NoActiveProof)?;
        if !proof.checkpoints.contains_key(&id) {
            return Err(CheckpointError::Unknown);
        }
        proof.current = id;
        Ok(())
    }

    pub(crate) fn steps_back(&self, steps: u64) -> Result<CheckpointId, CheckpointError> {
        if steps == 0 {
            return Err(CheckpointError::TooFar);
        }
        let proof = self.proof.as_ref().ok_or(CheckpointError::NoActiveProof)?;
        let mut id = proof.current;
        for _ in 0..steps {
            id = proof
                .checkpoints
                .get(&id)
                .ok_or(CheckpointError::Unknown)?
                .parent
                .ok_or(CheckpointError::TooFar)?;
        }
        Ok(id)
    }

    pub(crate) fn clear(&mut self) -> Option<ProofSession> {
        self.proof.take()
    }

    /// Forget only process-local PET handles after a child loss or workspace
    /// epoch transition. Topology and exact accepted inputs remain replayable.
    pub(crate) fn invalidate_states(&mut self) {
        if let Some(proof) = &mut self.proof {
            for checkpoint in proof.checkpoints.values_mut() {
                checkpoint.pet_state = None;
            }
        }
    }

    /// Return the unique root-to-node path, including both endpoints.
    pub(crate) fn path(&self, target: CheckpointId) -> Result<Vec<CheckpointId>, CheckpointError> {
        let proof = self.proof.as_ref().ok_or(CheckpointError::NoActiveProof)?;
        let mut path = Vec::new();
        let mut cursor = target;
        loop {
            let checkpoint = proof
                .checkpoints
                .get(&cursor)
                .ok_or(CheckpointError::Unknown)?;
            path.push(cursor);
            match checkpoint.parent {
                Some(parent) => cursor = parent,
                None => break,
            }
        }
        path.reverse();
        Ok(path)
    }

    pub(crate) fn fragments(&self, target: CheckpointId) -> Result<Vec<String>, CheckpointError> {
        let mut fragments = Vec::new();
        for checkpoint in self.path(target)? {
            if let Some(input) = &self.lookup(checkpoint)?.accepted_input {
                fragments.push(input.clone());
            }
        }
        Ok(fragments)
    }

    fn allocate(&mut self) -> Result<CheckpointId, CheckpointError> {
        self.next = self.next.checked_add(1).ok_or(CheckpointError::Exhausted)?;
        Ok(CheckpointId(self.next))
    }
}
