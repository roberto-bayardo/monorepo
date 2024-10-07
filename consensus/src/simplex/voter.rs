//! TODO: change name to voter

use super::{orchestrator::Orchestrator, wire};
use crate::{simplex::orchestrator::Proposal, Application, Hash};
use bytes::Bytes;
use commonware_cryptography::{bls12381::dkg::utils::threshold, utils::hex, PublicKey, Scheme};
use commonware_runtime::Clock;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, SystemTime},
};
use tracing::{debug, trace, warn};

// TODO: move to config
const PROPOSAL_NAMESPACE: &[u8] = b"_COMMONWARE_CONSENSUS_SIMPLEX_PROPOSAL_";
const VOTE_NAMESPACE: &[u8] = b"_COMMONWARE_CONSENSUS_SIMPLEX_VOTE_";
const FINALIZE_NAMESPACE: &[u8] = b"_COMMONWARE_CONSENSUS_SIMPLEX_FINALIZE_";

// TODO: move to shared location
pub fn proposal_digest(view: u64, height: u64, parent: Bytes, payload_hash: Bytes) -> Bytes {
    let mut msg = Vec::new();
    msg.extend_from_slice(&view.to_be_bytes());
    msg.extend_from_slice(&height.to_be_bytes());
    msg.extend_from_slice(&parent);
    msg.extend_from_slice(&payload_hash);
    msg.into()
}

fn vote_digest(view: u64, height: u64, proposal_hash: Bytes) -> Bytes {
    let mut msg = Vec::new();
    msg.extend_from_slice(&view.to_be_bytes());
    msg.extend_from_slice(&height.to_be_bytes());
    msg.extend_from_slice(&proposal_hash);
    msg.into()
}

fn finalize_digest(view: u64, height: u64, proposal_hash: Bytes) -> Bytes {
    let mut msg = Vec::new();
    msg.extend_from_slice(&view.to_be_bytes());
    msg.extend_from_slice(&height.to_be_bytes());
    msg.extend_from_slice(&proposal_hash);
    msg.into()
}

pub fn hash(bytes: Bytes) -> Bytes {
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hasher.finalize().to_vec().into()
}

pub struct View {
    idx: u64,

    leader: PublicKey,
    leader_deadline: Option<SystemTime>,
    advance_deadline: Option<SystemTime>,
    null_vote_retry: Option<SystemTime>,

    // Track one proposal per view
    proposal: Option<(Hash /* proposal */, wire::Proposal)>,
    broadcast_vote: bool,
    broadcast_finalize: bool,

    // Track votes for all proposals (ensuring any participant only has one recorded vote)
    proposal_voters: HashSet<PublicKey>,
    proposal_votes: HashMap<Hash, HashMap<PublicKey, wire::Vote>>,
    broadcast_proposal_notarization: bool,

    timeout_fired: bool,
    null_votes: HashMap<PublicKey, wire::Vote>,
    broadcast_null_notarization: bool,

    // Track finalizes for all proposals (ensuring any participant only has one recorded finalize)
    finalizers: HashSet<PublicKey>,
    finalizes: HashMap<Hash, HashMap<PublicKey, wire::Finalize>>,
    broadcast_finalization: bool,
}

impl View {
    pub fn new(
        idx: u64,
        leader: PublicKey,
        leader_deadline: Option<SystemTime>,
        advance_deadline: Option<SystemTime>,
    ) -> Self {
        Self {
            idx,

            leader,
            leader_deadline,
            advance_deadline,
            null_vote_retry: None,

            proposal: None,
            broadcast_vote: false,
            broadcast_finalize: false,

            proposal_voters: HashSet::new(),
            proposal_votes: HashMap::new(),
            broadcast_proposal_notarization: false,

            timeout_fired: false,
            null_votes: HashMap::new(),
            broadcast_null_notarization: false,

            finalizers: HashSet::new(),
            finalizes: HashMap::new(),
            broadcast_finalization: false,
        }
    }

    fn add_verified_vote(&mut self, vote: wire::Vote) {
        if vote.hash.len() == 0 {
            self.null_votes
                .insert(vote.signature.as_ref().unwrap().public_key.clone(), vote);
            return;
        }

        // Check if already voted
        let public_key = &vote.signature.as_ref().unwrap().public_key;
        if self.proposal_voters.contains(public_key) {
            return;
        }

        // Store the vote
        self.proposal_voters.insert(public_key.clone());
        let entry = self
            .proposal_votes
            .entry(vote.hash.clone())
            .or_insert_with(HashMap::new);
        entry.insert(public_key.clone(), vote);
    }

    fn notarizable_proposal(
        &mut self,
        threshold: u32,
    ) -> Option<(Hash, &HashMap<PublicKey, wire::Vote>)> {
        if self.broadcast_proposal_notarization {
            return None;
        }
        for (proposal, votes) in self.proposal_votes.iter() {
            if (votes.len() as u32) < threshold {
                continue;
            }

            // Ensure we have the proposal we are going to broadcast a notarization for
            match &self.proposal {
                Some((hash, _)) => {
                    if hash != proposal {
                        debug!(
                            proposal = hex(&proposal),
                            hash = hex(&hash),
                            reason = "proposal mismatch",
                            "skipping notarization broadcast"
                        );
                        continue;
                    }
                }
                None => {
                    continue;
                }
            }

            // There should never exist enough votes for multiple proposals, so it doesn't
            // matter which one we choose.
            self.broadcast_proposal_notarization = true;
            return Some((proposal.clone(), votes));
        }
        None
    }

    fn notarizable_null(
        &mut self,
        threshold: u32,
    ) -> Option<(Hash, &HashMap<PublicKey, wire::Vote>)> {
        if self.broadcast_null_notarization {
            return None;
        }
        if (self.null_votes.len() as u32) < threshold {
            return None;
        }
        self.broadcast_null_notarization = true;
        Some((Bytes::new(), &self.null_votes))
    }

    fn add_verified_finalize(&mut self, finalize: wire::Finalize) {
        // Check if already finalized
        let public_key = &finalize.signature.as_ref().unwrap().public_key;
        if self.finalizers.contains(public_key) {
            return;
        }

        // Store the finalize
        self.finalizers.insert(public_key.clone());
        let entry = self
            .finalizes
            .entry(finalize.hash.clone())
            .or_insert_with(HashMap::new);
        entry.insert(public_key.clone(), finalize);
    }

    fn finalizable_proposal(
        &mut self,
        threshold: u32,
    ) -> Option<(Hash, &HashMap<PublicKey, wire::Finalize>)> {
        if self.broadcast_finalization {
            return None;
        }
        for (proposal, finalizes) in self.finalizes.iter() {
            if (finalizes.len() as u32) < threshold {
                continue;
            }

            // Ensure we have the proposal we are going to broadcast a finalization for
            match &self.proposal {
                Some((hash, _)) => {
                    if hash != proposal {
                        debug!(
                            proposal = hex(&proposal),
                            hash = hex(&hash),
                            reason = "proposal mismatch",
                            "skipping finalization broadcast"
                        );
                        continue;
                    }
                }
                None => {
                    continue;
                }
            }

            // There should never exist enough finalizes for multiple proposals, so it doesn't
            // matter which one we choose.
            self.broadcast_finalization = true;
            return Some((proposal.clone(), finalizes));
        }
        None
    }
}

// TODO: create fault tracker that can be configured by developer to do something

pub struct Voter<E: Clock + Rng, C: Scheme, A: Application> {
    runtime: E,
    crypto: C,

    orchestrator: Orchestrator<E, A>,

    threshold: u32,
    validators: Vec<PublicKey>,
    validators_ordered: HashMap<PublicKey, u32>,

    view: u64,
    views: HashMap<u64, View>,
}

impl<E: Clock + Rng, C: Scheme, A: Application> Voter<E, C, A> {
    pub fn new(
        runtime: E,
        crypto: C,
        orchestrator: Orchestrator<E, A>,
        mut validators: Vec<PublicKey>,
    ) -> Self {
        // Initialize ordered validators
        validators.sort();
        let mut validators_ordered = HashMap::new();
        for (i, validator) in validators.iter().enumerate() {
            validators_ordered.insert(validator.clone(), i as u32);
        }

        // Initialize store
        Self {
            runtime,
            crypto,

            orchestrator,

            // TODO: move this helper
            threshold: threshold(validators.len() as u32)
                .expect("not possible to satisfy 2f+1 threshold"),
            validators,
            validators_ordered,

            view: 0,
            views: HashMap::new(),
        }
    }

    pub fn propose(&mut self) -> Option<wire::Proposal> {
        // Check if we are leader
        let view = self.views.get(&self.view).unwrap();
        if view.leader != self.crypto.public_key() {
            return None;
        }

        // Check if we have already proposed
        if view.proposal.is_some() {
            return None;
        }

        // Select parent block
        let (parent, payload) = match self.orchestrator.propose() {
            Some((parent, payload)) => (parent, payload),
            None => {
                debug!(reason = "no available parent", "dropping proposal");
                return None;
            }
        };
        let height = parent.1.height + 1;

        // Get payload hash
        let payload_hash = match self.orchestrator.parse(payload.clone()) {
            Some(hash) => hash,
            None => {
                warn!(
                    reason = "invalid payload produced by self",
                    "dropping proposal"
                );
                return None;
            }
        };

        // Construct proposal
        let digest = proposal_digest(self.view, height, parent.0.clone(), payload_hash.clone());
        let proposal = wire::Proposal {
            view: self.view,
            height,
            parent: parent.0,
            payload,
            signature: Some(wire::Signature {
                public_key: self.crypto.public_key(),
                signature: self.crypto.sign(PROPOSAL_NAMESPACE, &digest),
            }),
        };
        Some(proposal)
    }

    pub fn timeout_deadline(&mut self) -> SystemTime {
        // Return the earliest deadline
        let view = self.views.get_mut(&self.view).unwrap();
        if let Some(deadline) = view.leader_deadline {
            return deadline;
        }
        if let Some(deadline) = view.advance_deadline {
            return deadline;
        }

        // If no deadlines are still set (waiting for null votes),
        // return next try for null block vote
        if let Some(deadline) = view.null_vote_retry {
            return deadline;
        }

        // Set null vote retry, if none already set
        let null_vote_retry = self.runtime.current() + Duration::from_secs(30);
        view.null_vote_retry = Some(null_vote_retry);
        null_vote_retry
    }

    pub fn timeout(&mut self) -> wire::Vote {
        // Set timeout fired
        let view = self.views.get_mut(&self.view).unwrap();
        view.timeout_fired = true;

        // Remove deadlines
        view.leader_deadline = None;
        view.advance_deadline = None;
        view.null_vote_retry = None;

        // Construct null vote
        let hash = Bytes::new();
        let digest = vote_digest(self.view, 0, hash.clone());
        wire::Vote {
            view: self.view,
            height: 0,
            hash,
            signature: Some(wire::Signature {
                public_key: self.crypto.public_key(),
                signature: self.crypto.sign(VOTE_NAMESPACE, &digest),
            }),
        }
    }

    pub fn proposal(&mut self, proposal: wire::Proposal) {
        // Parse signature
        let signature = match &proposal.signature {
            Some(signature) => signature,
            _ => {
                debug!(reason = "missing signature", "dropping proposal");
                return;
            }
        };

        // Ensure we are in the right view to process this message
        if proposal.view != self.view && proposal.view != self.view + 1 {
            debug!(
                proposal_view = proposal.view,
                our_view = self.view,
                reason = "incorrect view",
                "dropping proposal"
            );
            return;
        }

        // Check expected leader
        let expected_leader =
            self.validators[proposal.view as usize % self.validators.len()].clone();
        if !C::validate(&signature.public_key) {
            debug!(reason = "invalid signature", "dropping proposal");
            return;
        }
        if expected_leader != signature.public_key {
            debug!(
                proposal_leader = hex(&signature.public_key),
                view_leader = hex(&expected_leader),
                reason = "leader mismatch",
                "dropping proposal"
            );
            return;
        }

        // Check to see if we have already received a proposal for this view (if exists)
        if let Some(view) = self.views.get(&proposal.view) {
            if view.proposal.is_some() {
                debug!(view = proposal.view, "proposal already exists");
                // TODO: check if different signed proposal and post fault
                return;
            }
        }

        // Verify the signature
        let payload_hash = match self.orchestrator.parse(proposal.payload.clone()) {
            Some(hash) => hash,
            None => {
                debug!(reason = "invalid payload", "dropping proposal");
                return;
            }
        };
        let proposal_digest = proposal_digest(
            proposal.view,
            proposal.height,
            proposal.parent.clone(),
            payload_hash.clone(),
        );
        if !C::verify(
            PROPOSAL_NAMESPACE,
            &proposal_digest,
            &signature.public_key,
            &signature.signature,
        ) {
            debug!(reason = "invalid signature", "dropping proposal");
            return;
        }

        // Verify the proposal
        if !self.orchestrator.verify(proposal.clone()) {
            debug!(reason = "invalid payload", "dropping proposal");
            return;
        };

        // Store the proposal
        let view = self
            .views
            .entry(proposal.view)
            .or_insert_with(|| View::new(proposal.view, expected_leader, None, None));
        let proposal_hash = hash(proposal_digest);
        view.proposal = Some((proposal_hash.clone(), proposal));
        view.leader_deadline = None;
    }

    fn enter_view(&mut self, view: u64) {
        // Ensure view is valid
        if view <= self.view {
            panic!("cannot enter previous or current view");
        }

        // Setup new view
        let entry = self.views.entry(view).or_insert_with(|| {
            View::new(
                view,
                self.validators[view as usize % self.validators.len()].clone(),
                None,
                None,
            )
        });
        entry.leader_deadline = Some(self.runtime.current() + Duration::from_secs(1));
        entry.advance_deadline = Some(self.runtime.current() + Duration::from_secs(2));

        // TODO: prune old views once finalized is above
    }

    pub fn vote(&mut self, vote: wire::Vote) {
        // Ensure we are in the right view to process this message
        if vote.view != self.view && vote.view != self.view + 1 {
            debug!(
                vote_view = vote.view,
                our_view = self.view,
                reason = "incorrect view",
                "dropping vote"
            );
            return;
        }

        // Parse signature
        let signature = match &vote.signature {
            Some(signature) => signature,
            _ => {
                debug!(reason = "missing signature", "dropping vote");
                return;
            }
        };
        if !C::validate(&signature.public_key) {
            debug!(reason = "invalid signature", "dropping vote");
            return;
        }

        // Verify that signer is a validator
        if !self.validators_ordered.contains_key(&signature.public_key) {
            debug!(
                signer = hex(&signature.public_key),
                reason = "invalid validator",
                "dropping vote"
            );
            return;
        }

        // Assert null vote is well-formed
        if vote.hash.len() == 0 && vote.height != 0 {
            debug!(reason = "null vote with non-zero height", "dropping vote");
            return;
        }

        // Verify the signature
        let vote_digest = vote_digest(vote.view, vote.height, vote.hash.clone());
        if !C::verify(
            VOTE_NAMESPACE,
            &vote_digest,
            &signature.public_key,
            &signature.signature,
        ) {
            debug!(reason = "invalid signature", "dropping vote");
            return;
        }

        // Check to see if vote is for proposal in view
        let view = self.views.entry(vote.view).or_insert_with(|| {
            View::new(
                vote.view,
                self.validators[vote.view as usize % self.validators.len()].clone(),
                None,
                None,
            )
        });

        // Check if already voted to finalize if null vote
        if vote.hash.len() == 0 && view.finalizers.contains(&signature.public_key) {
            debug!(
                signer = hex(&signature.public_key),
                reason = "already voted finalize",
                "dropping null vote"
            );
            return;
        }

        // Handle vote
        view.add_verified_vote(vote);
    }

    pub fn notarization(&mut self, notarization: wire::Notarization) {
        // Check if we are still in a view that this would help with
        if notarization.view < self.view {
            debug!(
                notarization_view = notarization.view,
                our_view = self.view,
                reason = "outdated notarization",
                "dropping notarization"
            );
            return;
        }

        // Assert null notarization is well-formed
        if notarization.hash.len() == 0 && notarization.height != 0 {
            debug!(
                reason = "null notation with non-zero height",
                "dropping notarization"
            );
            return;
        }

        // Lookup view in case we can add any missing signatures
        let mut view = self.views.get_mut(&notarization.view);

        // Verify threshold notarization
        let mut added = 0;
        let mut seen = HashSet::new();
        for signature in notarization.signatures {
            // Verify signature
            if !C::validate(&signature.public_key) {
                debug!(
                    signer = hex(&signature.public_key),
                    reason = "invalid validator",
                    "dropping notarization"
                );
                return;
            }

            // Ensure we haven't seen this signature before
            if seen.contains(&signature.public_key) {
                debug!(
                    signer = hex(&signature.public_key),
                    reason = "duplicate signature",
                    "dropping notarization"
                );
                return;
            }
            seen.insert(signature.public_key.clone());

            // Verify signature
            if !C::verify(
                VOTE_NAMESPACE,
                &vote_digest(
                    notarization.view,
                    notarization.height,
                    notarization.hash.clone(),
                ),
                &signature.public_key,
                &signature.signature,
            ) {
                debug!(reason = "invalid signature", "dropping notarization");
                return;
            }

            // Add any useful signatures
            if let Some(ref mut view) = view {
                // Check if null and already finalized
                if notarization.hash.len() == 0 && view.finalizers.contains(&signature.public_key) {
                    debug!(
                        signer = hex(&signature.public_key),
                        reason = "already voted finalize",
                        "dropping notarization"
                    );
                    // TODO: fault
                } else {
                    // Store vote
                    view.add_verified_vote(wire::Vote {
                        view: notarization.view,
                        height: notarization.height,
                        hash: notarization.hash.clone(),
                        signature: Some(signature),
                    });
                }
            }

            // Track that we added one for threshold
            added += 1;
        }
        if added <= self.threshold {
            debug!(
                threshold = self.threshold,
                signatures = added,
                reason = "insufficient signatures",
                "dropping notarization"
            );
            return;
        }
        debug!(view = notarization.view, added, "notarization verified");

        // Inform orchestrator of notarization
        let proposal = match view.and_then(|view| view.proposal.as_ref()) {
            Some((hash, proposal)) => Proposal::Populated(hash.clone(), proposal.clone()),
            None => Proposal::Reference(
                notarization.view,
                notarization.height,
                notarization.hash.clone(),
            ),
        };
        self.orchestrator.notarized(proposal);

        // Enter next view
        self.enter_view(notarization.view + 1);
    }

    pub fn finalize(&mut self, finalize: wire::Finalize) {
        // Ensure we are in the right view to process this message
        if finalize.view != self.view && finalize.view != self.view + 1 {
            debug!(
                finalize_view = finalize.view,
                our_view = self.view,
                reason = "incorrect view",
                "dropping finalize"
            );
            return;
        }

        // Parse signature
        let signature = match &finalize.signature {
            Some(signature) => signature,
            _ => {
                debug!(reason = "missing signature", "dropping finalize");
                return;
            }
        };
        if !C::validate(&signature.public_key) {
            debug!(reason = "invalid signature", "dropping finalize");
            return;
        }

        // Verify that signer is a validator
        if !self.validators_ordered.contains_key(&signature.public_key) {
            debug!(
                signer = hex(&signature.public_key),
                reason = "invalid validator",
                "dropping finalize"
            );
            return;
        }

        // Verify the signature
        let finalize_digest =
            finalize_digest(finalize.view, finalize.height, finalize.hash.clone());
        if !C::verify(
            VOTE_NAMESPACE,
            &finalize_digest,
            &signature.public_key,
            &signature.signature,
        ) {
            debug!(reason = "invalid signature", "dropping vote");
            return;
        }

        // Get view for finalize
        let view = self.views.entry(finalize.view).or_insert_with(|| {
            View::new(
                finalize.view,
                self.validators[finalize.view as usize % self.validators.len()].clone(),
                None,
                None,
            )
        });

        // Check if finalize vote is for a block (Fault)
        if finalize.hash.len() == 0 {
            // TODO: record fault
            debug!(reason = "finalize for null block", "dropping finalize");
            return;
        }

        // Check if already votes for null (Fault)
        if view.null_votes.contains_key(&signature.public_key) {
            debug!(
                signer = hex(&signature.public_key),
                reason = "already voted null",
                "dropping finalize"
            );
            return;
        }

        // Handle finalize
        view.add_verified_finalize(finalize);
    }

    pub fn finalization(&mut self, finalization: wire::Finalization) {
        // Ensure not for null (should never happen)
        if finalization.hash.len() == 0 {
            debug!(reason = "finalize for null block", "dropping finalization");
            // TODO: record faults
            return;
        }

        // Lookup view in case we can add any missing signatures
        let mut view = self.views.get_mut(&finalization.view);

        // Verify threshold finalization
        let mut added = 0;
        let mut seen = HashSet::new();
        for signature in finalization.signatures {
            // Verify signature
            if !C::validate(&signature.public_key) {
                debug!(
                    signer = hex(&signature.public_key),
                    reason = "invalid validator",
                    "dropping finalization"
                );
                return;
            }

            // Ensure we haven't seen this signature before
            if seen.contains(&signature.public_key) {
                debug!(
                    signer = hex(&signature.public_key),
                    reason = "duplicate signature",
                    "dropping finalization"
                );
                return;
            }
            seen.insert(signature.public_key.clone());

            // Verify signature
            if !C::verify(
                FINALIZE_NAMESPACE,
                &finalize_digest(
                    finalization.view,
                    finalization.height,
                    finalization.hash.clone(),
                ),
                &signature.public_key,
                &signature.signature,
            ) {
                debug!(reason = "invalid signature", "dropping finalization");
                return;
            }

            // Add any useful signatures
            if let Some(ref mut view) = view {
                // Check if already voted for null
                if view.null_votes.contains_key(&signature.public_key) {
                    debug!(
                        signer = hex(&signature.public_key),
                        reason = "already voted null",
                        "dropping finalization"
                    );
                } else {
                    // Store finalize
                    view.add_verified_finalize(wire::Finalize {
                        view: finalization.view,
                        height: finalization.height,
                        hash: finalization.hash.clone(),
                        signature: Some(signature),
                    });
                }
            }

            // Track that we added one for threshold
            added += 1;
        }
        debug!(view = finalization.view, added, "finalization verified");

        // Inform orchestrator of finalization
        let proposal = match view.and_then(|view| view.proposal.as_ref()) {
            Some((hash, proposal)) => Proposal::Populated(hash.clone(), proposal.clone()),
            None => Proposal::Reference(
                finalization.view,
                finalization.height,
                finalization.hash.clone(),
            ),
        };
        self.orchestrator.finalized(proposal);

        // Enter next view (if applicable)
        if finalization.view >= self.view {
            self.enter_view(finalization.view + 1);
        }
    }

    pub fn construct_vote() -> Option<wire::Vote> {}

    pub fn construct_notarization(
        validators: &Vec<PublicKey>,
        threshold: u32,
        view: &mut View,
    ) -> Option<wire::Notarization> {
        // Determine which votes to use
        let (proposal_hash, votes) = match last_vote_null {
            true => view.notarizable_null(threshold)?,
            false => view.notarizable_proposal(threshold)?,
        };

        // Construct notarization
        let mut signatures = Vec::new();
        for validator in validators.iter() {
            if let Some(vote) = votes.get(validator) {
                signatures.push(vote.signature.clone().unwrap());
            }
        }
        let notarization = wire::Notarization {
            view: view.idx,
            block: proposal_hash,
            signatures,
        };
        Some(notarization)
    }

    pub fn construct_finalize() -> Option<wire::Finalize> {}

    fn construct_finalization(
        validators: &Vec<PublicKey>,
        threshold: u32,
        view: &mut View,
    ) -> Option<wire::Finalization> {
        // Check if we have enough finalizes
        let (proposal_hash, finalizes) = view.finalizable_proposal(threshold)?;

        // Construct finalization
        let mut signatures = Vec::new();
        for validator in validators.iter() {
            if let Some(finalize) = finalizes.get(validator) {
                signatures.push(finalize.signature.clone().unwrap());
            }
        }
        let finalization = wire::Finalization {
            view: view.idx,
            block: proposal_hash,
            signatures,
        };
        Some(finalization)
    }
}