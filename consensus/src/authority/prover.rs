//! Prover module for the authority node.
//!
//! We don't use protobuf for proof encoding because we expect external parties
//! to decode proofs in constrained environments where protobuf may not be implemented.

use super::{
    encoder::{
        finalize_message, finalize_namespace, proposal_message, proposal_namespace, vote_message,
        vote_namespace,
    },
    wire, Height, View,
};
use crate::Proof;
use bytes::{Buf, BufMut, Bytes};
use commonware_cryptography::{Digest, Hasher, PublicKey, Scheme};
use core::panic;
use std::marker::PhantomData;

#[derive(Clone)]
pub struct Prover<C: Scheme, H: Hasher> {
    _crypto: PhantomData<C>,
    hasher: H,

    proposal_namespace: Vec<u8>,
    vote_namespace: Vec<u8>,
    finalize_namespace: Vec<u8>,
}

impl<C: Scheme, H: Hasher> Prover<C, H> {
    pub fn new(hasher: H, namespace: Bytes) -> Self {
        Self {
            _crypto: PhantomData,
            hasher,
            proposal_namespace: proposal_namespace(&namespace),
            vote_namespace: vote_namespace(&namespace),
            finalize_namespace: finalize_namespace(&namespace),
        }
    }

    pub(crate) fn serialize_proposal(
        view: View,
        height: Height,
        parent: Digest,
        payload: Digest,
        signature: wire::Signature,
    ) -> Proof {
        // Setup proof
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        let len = 8 + 8 + digest_len + digest_len + public_key_len + signature_len;
        let mut proof = Vec::with_capacity(len);

        // Encode proof
        proof.put_u64(view);
        proof.put_u64(height);
        proof.put(parent);
        proof.put(payload);
        proof.put(signature.public_key);
        proof.put(signature.signature);
        proof.into()
    }

    pub fn deserialize_proposal(
        &mut self,
        mut proof: Proof,
        check_sig: bool,
    ) -> Option<(PublicKey, View, Height, Digest)> {
        // Ensure proof is big enough
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        if proof.len() != 8 + 8 + digest_len + digest_len + public_key_len + signature_len {
            return None;
        }

        // Decode proof
        let view = proof.get_u64();
        let height = proof.get_u64();
        let parent = proof.copy_to_bytes(digest_len);
        let payload = proof.copy_to_bytes(digest_len);
        let public_key = proof.copy_to_bytes(public_key_len);
        let signature = proof.copy_to_bytes(signature_len);

        // Verify signature
        let proposal_message = proposal_message(view, height, &parent, &payload);
        if check_sig {
            if !C::validate(&public_key) {
                return None;
            }
            if !C::verify(
                &self.proposal_namespace,
                &proposal_message,
                &public_key,
                &signature,
            ) {
                return None;
            }
        }

        // Compute digest
        self.hasher.update(&proposal_message);
        Some((public_key, view, height, self.hasher.finalize()))
    }

    pub(crate) fn serialize_vote(vote: wire::Vote) -> Proof {
        // Setup proof
        let (public_key_len, signature_len) = C::len();
        let len = 8 + 8 + H::len() + public_key_len + signature_len;
        let mut proof = Vec::with_capacity(len);

        // Encode proofs
        proof.put_u64(vote.view);
        proof.put_u64(vote.height.expect("height not populated"));
        proof.put(vote.digest.expect("digest not populated"));
        let signature = vote.signature.expect("signature not populated");
        proof.put(signature.public_key);
        proof.put(signature.signature);
        proof.into()
    }

    pub fn deserialize_vote(
        &self,
        mut proof: Proof,
        check_sig: bool,
    ) -> Option<(PublicKey, View, Height, Digest)> {
        // Ensure proof is big enough
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        if proof.len() != 8 + 8 + digest_len + public_key_len + signature_len {
            return None;
        }

        // Decode proof
        let view = proof.get_u64();
        let height = proof.get_u64();
        let digest = proof.copy_to_bytes(digest_len);
        let public_key = proof.copy_to_bytes(public_key_len);
        let signature = proof.copy_to_bytes(signature_len);

        // Verify signature
        if check_sig {
            if !C::validate(&public_key) {
                return None;
            }
            let vote_message = vote_message(view, Some(height), Some(&digest));
            if !C::verify(&self.vote_namespace, &vote_message, &public_key, &signature) {
                return None;
            }
        }
        Some((public_key, view, height, digest))
    }

    pub(crate) fn serialize_finalize(finalize: wire::Finalize) -> Proof {
        // Setup proof
        let (public_key_len, signature_len) = C::len();
        let len = 8 + 8 + H::len() + public_key_len + signature_len;
        let mut proof = Vec::with_capacity(len);

        // Encode proof
        proof.put_u64(finalize.view);
        proof.put_u64(finalize.height);
        proof.put(finalize.digest);
        let signature = finalize.signature.expect("signature not populated");
        proof.put(signature.public_key);
        proof.put(signature.signature);
        proof.into()
    }

    pub fn deserialize_finalize(
        &self,
        mut proof: Proof,
        check_sig: bool,
    ) -> Option<(PublicKey, View, Height, Digest)> {
        // Ensure proof is big enough
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        if proof.len() != 8 + 8 + digest_len + public_key_len + signature_len {
            return None;
        }

        // Decode proof
        let view = proof.get_u64();
        let height = proof.get_u64();
        let digest = proof.copy_to_bytes(digest_len);
        let public_key = proof.copy_to_bytes(public_key_len);
        let signature = proof.copy_to_bytes(signature_len);

        // Verify signature
        if check_sig {
            if !C::validate(&public_key) {
                return None;
            }
            let finalize_message = finalize_message(view, height, &digest);
            if !C::verify(
                &self.finalize_namespace,
                &finalize_message,
                &public_key,
                &signature,
            ) {
                return None;
            }
        }
        Some((public_key, view, height, digest))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn serialize_conflicting_proposal(
        view: View,
        height_1: Height,
        parent_1: Digest,
        payload_1: Digest,
        signature_1: wire::Signature,
        height_2: Height,
        parent_2: Digest,
        payload_2: Digest,
        signature_2: wire::Signature,
    ) -> Proof {
        // Setup proof
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        let len = 8
            + public_key_len
            + 8
            + digest_len
            + digest_len
            + signature_len
            + 8
            + digest_len
            + digest_len
            + signature_len;

        // Ensure proof can be generated correctly
        if signature_1.public_key != signature_2.public_key {
            panic!("public keys do not match");
        }
        let public_key = signature_1.public_key;

        // Encode proof
        let mut proof = Vec::with_capacity(len);
        proof.put_u64(view);
        proof.put(public_key);
        proof.put_u64(height_1);
        proof.put(parent_1);
        proof.put(payload_1);
        proof.put(signature_1.signature);
        proof.put_u64(height_2);
        proof.put(parent_2);
        proof.put(payload_2);
        proof.put(signature_2.signature);
        proof.into()
    }

    pub fn deserialize_conflicting_proposal(
        &self,
        mut proof: Proof,
        check_sig: bool,
    ) -> Option<(PublicKey, View)> {
        // Ensure proof is big enough
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        let len = 8
            + public_key_len
            + 8
            + digest_len
            + digest_len
            + signature_len
            + 8
            + digest_len
            + digest_len
            + signature_len;
        if proof.len() != len {
            return None;
        }

        // Decode proof
        let view = proof.get_u64();
        let public_key = proof.copy_to_bytes(public_key_len);
        let height_1 = proof.get_u64();
        let parent_1 = proof.copy_to_bytes(digest_len);
        let payload_1 = proof.copy_to_bytes(digest_len);
        let signature_1 = proof.copy_to_bytes(signature_len);
        let height_2 = proof.get_u64();
        let parent_2 = proof.copy_to_bytes(digest_len);
        let payload_2 = proof.copy_to_bytes(digest_len);
        let signature_2 = proof.copy_to_bytes(signature_len);

        // Verify signatures
        if check_sig {
            if !C::validate(&public_key) {
                return None;
            }
            let proposal_message_1 = proposal_message(view, height_1, &parent_1, &payload_1);
            let proposal_message_2 = proposal_message(view, height_2, &parent_2, &payload_2);
            if !C::verify(
                &self.proposal_namespace,
                &proposal_message_1,
                &public_key,
                &signature_1,
            ) || !C::verify(
                &self.proposal_namespace,
                &proposal_message_2,
                &public_key,
                &signature_2,
            ) {
                return None;
            }
        }
        Some((public_key, view))
    }

    pub(crate) fn serialize_conflicting_vote(
        view: View,
        height_1: Height,
        hash_1: Digest,
        signature_1: wire::Signature,
        height_2: Height,
        hash_2: Digest,
        signature_2: wire::Signature,
    ) -> Proof {
        // Setup proof
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        let len =
            8 + public_key_len + 8 + digest_len + signature_len + 8 + digest_len + signature_len;

        // Ensure proof can be generated correctly
        if signature_1.public_key != signature_2.public_key {
            panic!("public keys do not match");
        }
        let public_key = signature_1.public_key;

        // Encode proof
        let mut proof = Vec::with_capacity(len);
        proof.put_u64(view);
        proof.put(public_key);
        proof.put_u64(height_1);
        proof.put(hash_1);
        proof.put(signature_1.signature);
        proof.put_u64(height_2);
        proof.put(hash_2);
        proof.put(signature_2.signature);
        proof.into()
    }

    pub fn deserialize_conflicting_vote(
        &self,
        mut proof: Proof,
        check_sig: bool,
    ) -> Option<(PublicKey, View)> {
        // Ensure proof is big enough
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        let len =
            8 + public_key_len + 8 + digest_len + signature_len + 8 + digest_len + signature_len;
        if proof.len() != len {
            return None;
        }

        // Decode proof
        let view = proof.get_u64();
        let public_key = proof.copy_to_bytes(public_key_len);
        let height_1 = proof.get_u64();
        let hash_1 = proof.copy_to_bytes(digest_len);
        let signature_1 = proof.copy_to_bytes(signature_len);
        let height_2 = proof.get_u64();
        let hash_2 = proof.copy_to_bytes(digest_len);
        let signature_2 = proof.copy_to_bytes(signature_len);

        // Verify signatures
        if check_sig {
            if !C::validate(&public_key) {
                return None;
            }
            let vote_message_1 = vote_message(view, Some(height_1), Some(&hash_1));
            let vote_message_2 = vote_message(view, Some(height_2), Some(&hash_2));
            if !C::verify(
                &self.vote_namespace,
                &vote_message_1,
                &public_key,
                &signature_1,
            ) || !C::verify(
                &self.vote_namespace,
                &vote_message_2,
                &public_key,
                &signature_2,
            ) {
                return None;
            }
        }
        Some((public_key, view))
    }

    pub(crate) fn serialize_conflicting_finalize(
        view: View,
        height_1: Height,
        hash_1: Digest,
        signature_1: wire::Signature,
        height_2: Height,
        hash_2: Digest,
        signature_2: wire::Signature,
    ) -> Proof {
        // Setup proof
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        let len =
            8 + public_key_len + 8 + digest_len + signature_len + 8 + digest_len + signature_len;

        // Ensure proof can be generated correctly
        if signature_1.public_key != signature_2.public_key {
            panic!("public keys do not match");
        }
        let public_key = signature_1.public_key;

        // Encode proof
        let mut proof = Vec::with_capacity(len);
        proof.put_u64(view);
        proof.put(public_key);
        proof.put_u64(height_1);
        proof.put(hash_1);
        proof.put(signature_1.signature);
        proof.put_u64(height_2);
        proof.put(hash_2);
        proof.put(signature_2.signature);
        proof.into()
    }

    pub fn deserialize_conflicting_finalize(
        &self,
        mut proof: Proof,
        check_sig: bool,
    ) -> Option<(PublicKey, View)> {
        let digest_len = H::len();
        let (public_key_len, signature_len) = C::len();
        let len =
            8 + public_key_len + 8 + digest_len + signature_len + 8 + digest_len + signature_len;
        if proof.len() != len {
            return None;
        }

        // Decode proof
        let view = proof.get_u64();
        let public_key = proof.copy_to_bytes(public_key_len);
        let height_1 = proof.get_u64();
        let hash_1 = proof.copy_to_bytes(digest_len);
        let signature_1 = proof.copy_to_bytes(signature_len);
        let height_2 = proof.get_u64();
        let hash_2 = proof.copy_to_bytes(digest_len);
        let signature_2 = proof.copy_to_bytes(signature_len);

        // Verify signatures
        if check_sig {
            if !C::validate(&public_key) {
                return None;
            }
            let finalize_message_1 = finalize_message(view, height_1, &hash_1);
            let finalize_message_2 = finalize_message(view, height_2, &hash_2);
            if !C::verify(
                &self.finalize_namespace,
                &finalize_message_1,
                &public_key,
                &signature_1,
            ) || !C::verify(
                &self.finalize_namespace,
                &finalize_message_2,
                &public_key,
                &signature_2,
            ) {
                return None;
            }
        }
        Some((public_key, view))
    }

    pub(crate) fn serialize_null_finalize(
        view: View,
        height: Height,
        digest: Digest,
        signature_finalize: wire::Signature,
        signature_null: wire::Signature,
    ) -> Proof {
        // Setup proof
        let (public_key_len, signature_len) = C::len();
        let len = 8 + public_key_len + 8 + H::len() + signature_len + signature_len;

        // Ensure proof can be generated correctly
        if signature_finalize.public_key != signature_null.public_key {
            panic!("public keys do not match");
        }
        let public_key = signature_finalize.public_key;

        // Encode proof
        let mut proof = Vec::with_capacity(len);
        proof.put_u64(view);
        proof.put(public_key);
        proof.put_u64(height);
        proof.put(digest);
        proof.put(signature_finalize.signature);
        proof.put(signature_null.signature);
        proof.into()
    }

    pub fn deserialize_null_finalize(
        &self,
        mut proof: Proof,
        check_sig: bool,
    ) -> Option<(PublicKey, View)> {
        // Ensure proof is big enough
        let (public_key_len, signature_len) = C::len();
        let len = 8 + public_key_len + 8 + H::len() + signature_len + signature_len;
        if proof.len() != len {
            return None;
        }

        // Decode proof
        let view = proof.get_u64();
        let public_key = proof.copy_to_bytes(public_key_len);
        let height = proof.get_u64();
        let digest = proof.copy_to_bytes(H::len());
        let signature_finalize = proof.copy_to_bytes(signature_len);
        let signature_null = proof.copy_to_bytes(signature_len);

        // Verify signatures
        if check_sig {
            if !C::validate(&public_key) {
                return None;
            }
            let finalize_message = finalize_message(view, height, &digest);
            let null_message = vote_message(view, None, None);
            if !C::verify(
                &self.finalize_namespace,
                &finalize_message,
                &public_key,
                &signature_finalize,
            ) || !C::verify(
                &self.vote_namespace,
                &null_message,
                &public_key,
                &signature_null,
            ) {
                return None;
            }
        }
        Some((public_key, view))
    }
}