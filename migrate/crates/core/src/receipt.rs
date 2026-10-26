//! The signed receipt (architecture A4.3, implementation I4.1): the proof that the target equals
//! the source at the freeze point. A blake3 Merkle root over the source's per-chunk hashes must
//! equal the root over the target's, and the whole receipt is signed with the receiver's ed25519
//! key — so anyone holding both disks can recompute the roots and check the signature (USP U1).
//!
//! Pure: the key pair and the disks live in the receiver; here we build a receipt, compute roots,
//! sign and verify, deterministically, so every rule is testable with a fixed key.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// A binary Merkle tree over the ordered chunk hashes, blake3 at every node. An odd node at a
/// level is promoted by hashing it with itself, so the shape is fixed by the leaf count.
pub fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return *blake3::hash(b"bootshift-migrate/empty").as_bytes();
    }
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&pair[0]);
            hasher.update(pair.get(1).unwrap_or(&pair[0]));
            next.push(*hasher.finalize().as_bytes());
        }
        level = next;
    }
    level.first().copied().unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub format: String,
    pub run_id: String,
    pub epoch: u64,
    pub source_len: u64,
    pub target_len: u64,
    pub chunk_size: u32,
    pub source_root: [u8; 32],
    pub target_root: [u8; 32],
    pub frozen_at: String,
    pub committed_at: String,
    pub data_downtime_ms: u64,
    pub receiver_key: [u8; 32],
    /// The ed25519 signature over the receipt's digest; `None` until [`Receipt::sign`].
    pub signature: Option<Vec<u8>>,
}

impl Receipt {
    pub const FORMAT: &'static str = "bootshift-migrate-receipt/1";

    /// A fresh unsigned receipt. Timestamps are passed in (the core has no clock).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: String,
        epoch: u64,
        source_len: u64,
        target_len: u64,
        chunk_size: u32,
        source_root: [u8; 32],
        target_root: [u8; 32],
        frozen_at: String,
        committed_at: String,
        data_downtime_ms: u64,
    ) -> Self {
        Receipt {
            format: Receipt::FORMAT.to_string(),
            run_id,
            epoch,
            source_len,
            target_len,
            chunk_size,
            source_root,
            target_root,
            frozen_at,
            committed_at,
            data_downtime_ms,
            receiver_key: [0u8; 32],
            signature: None,
        }
    }

    /// blake3 over the receipt's canonical JSON with the signature cleared, so both sides compute
    /// the same digest to sign and to check. Struct field order is fixed, so the bytes are stable.
    pub fn digest(&self) -> [u8; 32] {
        let mut unsigned = self.clone();
        unsigned.signature = None;
        let json = serde_json::to_vec(&unsigned).unwrap_or_default();
        *blake3::hash(&json).as_bytes()
    }

    /// Records the signing key's public half and signs the digest.
    pub fn sign(&mut self, key: &SigningKey) {
        self.receiver_key = key.verifying_key().to_bytes();
        self.signature = None;
        let digest = self.digest();
        self.signature = Some(key.sign(&digest).to_bytes().to_vec());
    }

    /// Whether the signature is present, from `receiver_key`, and over this receipt's digest.
    pub fn verify(&self) -> bool {
        let Some(bytes) = self.signature.as_ref() else {
            return false;
        };
        let Ok(sig_bytes): Result<[u8; 64], _> = bytes.as_slice().try_into() else {
            return false;
        };
        let Ok(key) = VerifyingKey::from_bytes(&self.receiver_key) else {
            return false;
        };
        let signature = Signature::from_bytes(&sig_bytes);
        key.verify_strict(&self.digest(), &signature).is_ok()
    }

    /// The core claim: the source's root equals the target's.
    pub fn roots_match(&self) -> bool {
        self.source_root == self.target_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[42u8; 32])
    }

    fn sample(source_root: [u8; 32], target_root: [u8; 32]) -> Receipt {
        Receipt::new(
            "run-1".to_string(),
            7,
            1 << 30,
            2 << 30,
            1 << 20,
            source_root,
            target_root,
            "2026-10-27T00:00:00Z".to_string(),
            "2026-10-27T00:00:05Z".to_string(),
            5_000,
        )
    }

    #[test]
    fn the_merkle_root_is_deterministic_and_order_sensitive() {
        let a = merkle_root(&[leaf(1), leaf(2), leaf(3)]);
        let b = merkle_root(&[leaf(1), leaf(2), leaf(3)]);
        let c = merkle_root(&[leaf(3), leaf(2), leaf(1)]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn a_changed_leaf_changes_the_root() {
        let base = merkle_root(&[leaf(1), leaf(2), leaf(3), leaf(4)]);
        let changed = merkle_root(&[leaf(1), leaf(2), leaf(3), leaf(5)]);
        assert_ne!(base, changed);
    }

    #[test]
    fn a_signed_receipt_verifies_and_tampering_breaks_it() {
        let root = merkle_root(&[leaf(1), leaf(2)]);
        let mut receipt = sample(root, root);
        assert!(!receipt.verify(), "unsigned receipt does not verify");
        receipt.sign(&key());
        assert!(receipt.verify());
        assert!(receipt.roots_match());

        let mut tampered = receipt.clone();
        tampered.data_downtime_ms = 999;
        assert!(!tampered.verify(), "a changed field breaks the signature");
    }

    #[test]
    fn a_receipt_signed_by_another_key_does_not_verify_against_a_substituted_public_key() {
        let root = merkle_root(&[leaf(9)]);
        let mut receipt = sample(root, root);
        receipt.sign(&key());
        // Swap in a different public key but keep the old signature.
        receipt.receiver_key = SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(!receipt.verify());
    }

    #[test]
    fn mismatched_roots_are_reported() {
        let receipt = sample(merkle_root(&[leaf(1)]), merkle_root(&[leaf(2)]));
        assert!(!receipt.roots_match());
    }
}
