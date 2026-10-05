use interlock_schema::Criterion;
use sha2::{Digest, Sha256};

/// Version of the evidence policy in `evidence.rs`. Bumping it changes every
/// task's policy digest, so evidence gathered under the old policy stops counting.
pub const POLICY_VERSION: &str = "evidence-policy/v1";

/// The digest that goes into every currency key: the policy version plus the
/// task's criteria. Changing a criterion changes the digest.
pub fn policy_digest(criteria: &[Criterion]) -> String {
    let mut h = Sha256::new();
    h.update(POLICY_VERSION.as_bytes());
    h.update(b"\n");
    h.update(serde_json::to_vec(criteria).expect("criteria serialize"));
    format!("sha256:{}", hex::encode(h.finalize()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
