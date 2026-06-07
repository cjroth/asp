//! Node identity: an ed25519 keypair whose public key is the node's durable
//! identity (`site_id`/NodeId), serialized in OpenSSH public-key format for the
//! `authorized_keys` table (§Security). The same key signs the mutual-auth
//! handshake transcript and may optionally sign each row (`sig`, off by default).

use crate::error::{AspError, AspResult};
use crate::order::NodeId;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

#[derive(Clone)]
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    pub fn generate() -> Identity {
        use rand_core::OsRng;
        Identity { signing: SigningKey::generate(&mut OsRng) }
    }

    pub fn from_seed(seed: &[u8; 32]) -> Identity {
        Identity { signing: SigningKey::from_bytes(seed) }
    }

    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn node_id(&self) -> NodeId {
        NodeId(self.signing.verifying_key().to_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.signing.sign(msg).to_bytes().to_vec()
    }

    /// OpenSSH public-key line: `ssh-ed25519 <b64> <comment>`.
    pub fn to_ssh_string(&self) -> String {
        ssh_pubkey_string(&self.node_id(), "asp")
    }
}

/// OpenSSH `ssh-ed25519` public-key wire format, base64-encoded.
pub fn ssh_pubkey_string(node: &NodeId, comment: &str) -> String {
    let mut blob = Vec::new();
    let algo = b"ssh-ed25519";
    blob.extend_from_slice(&(algo.len() as u32).to_be_bytes());
    blob.extend_from_slice(algo);
    blob.extend_from_slice(&(node.0.len() as u32).to_be_bytes());
    blob.extend_from_slice(&node.0);
    format!(
        "ssh-ed25519 {} {}",
        base64::engine::general_purpose::STANDARD.encode(&blob),
        comment
    )
}

/// Parse an OpenSSH `ssh-ed25519 <b64> [comment]` line back to a NodeId.
pub fn parse_ssh_pubkey(line: &str) -> Option<NodeId> {
    let mut it = line.split_whitespace();
    if it.next()? != "ssh-ed25519" {
        return None;
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(it.next()?)
        .ok()?;
    let n = u32::from_be_bytes(blob.get(0..4)?.try_into().ok()?) as usize;
    let key_off = 4 + n + 4;
    let key = blob.get(key_off..key_off + 32)?;
    let mut a = [0u8; 32];
    a.copy_from_slice(key);
    Some(NodeId(a))
}

fn verifying_key(node: &NodeId) -> AspResult<VerifyingKey> {
    VerifyingKey::from_bytes(&node.0).map_err(|e| AspError::BadSignature(e.to_string()))
}

/// Verify a detached ed25519 signature (handshake transcript / optional row sig).
pub fn verify_detached(node: &NodeId, msg: &[u8], sig: &[u8]) -> AspResult<()> {
    let sig = Signature::from_slice(sig).map_err(|e| AspError::BadSignature(e.to_string()))?;
    verifying_key(node)?
        .verify(msg, &sig)
        .map_err(|e| AspError::BadSignature(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_roundtrip() {
        let id = Identity::from_seed(&[7u8; 32]);
        let line = id.to_ssh_string();
        assert!(line.starts_with("ssh-ed25519 "));
        assert_eq!(parse_ssh_pubkey(&line), Some(id.node_id()));
    }

    #[test]
    fn sign_verify() {
        let id = Identity::from_seed(&[3u8; 32]);
        let sig = id.sign(b"transcript");
        verify_detached(&id.node_id(), b"transcript", &sig).unwrap();
        assert!(verify_detached(&id.node_id(), b"other", &sig).is_err());
    }
}
