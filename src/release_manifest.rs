use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const MANIFEST_SCHEMA: &str = "claudometer-release-manifest-v1";
pub const SIGNATURE_SCHEMA: &str = "claudometer-release-signatures-v1";
pub const ROLLBACK_SCHEMA: &str = "claudometer-rollback-authorization-v1";
pub const RELEASE_CHANNEL: &str = "stable";

const MAX_SIGNATURES: usize = 2;
const CLOCK_SKEW_SECONDS: i64 = 5 * 60;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: String,
    pub channel: String,
    pub sequence: u64,
    pub version: String,
    pub tag: String,
    pub issued_at: String,
    pub policy_expires_at: String,
    pub architecture: String,
    pub asset: String,
    pub size: u64,
    pub sha256: String,
    pub minimum_updater_version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureEnvelope {
    schema: String,
    signatures: Vec<DetachedSignature>,
    #[serde(default)]
    next_public_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetachedSignature {
    public_key: String,
    signature: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RollbackAuthorization {
    schema: String,
    channel: String,
    from_sequence: u64,
    from_version: String,
    target_sequence: u64,
    target_version: String,
    target_tag: String,
    architecture: String,
    asset: String,
    issued_at: String,
    expires_at: String,
}

#[derive(Clone, Copy)]
pub struct VerificationPolicy<'a> {
    pub trusted_public_key: [u8; 32],
    pub channel: &'a str,
    pub architecture: &'a str,
    pub current_sequence: u64,
    pub current_version: (u16, u16, u16),
    pub now: OffsetDateTime,
    pub max_asset_size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRelease {
    pub manifest: ReleaseManifest,
    pub next_public_key: Option<[u8; 32]>,
}

#[derive(Clone, Copy)]
pub struct RollbackProof<'a> {
    pub bytes: &'a [u8],
    pub signatures: &'a [u8],
}

pub fn verify(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    rollback: Option<RollbackProof<'_>>,
    policy: VerificationPolicy<'_>,
) -> Result<VerifiedRelease, &'static str> {
    let next_public_key = verify_signatures(
        manifest_bytes,
        signature_bytes,
        &policy.trusted_public_key,
        true,
    )?;
    let manifest: ReleaseManifest =
        serde_json::from_slice(manifest_bytes).map_err(|_| "release manifest malformed")?;
    validate_manifest(&manifest, policy)?;

    let target_version = strict_version(&manifest.version).ok_or("release version malformed")?;
    if target_version < policy.current_version {
        let proof = rollback.ok_or("release downgrade not authorized")?;
        verify_rollback(proof, &manifest, policy)?;
    } else if target_version == policy.current_version {
        return Err("release version is not newer");
    } else if rollback.is_some() {
        return Err("unexpected rollback authorization");
    }

    Ok(VerifiedRelease {
        manifest,
        next_public_key,
    })
}

fn validate_manifest(
    manifest: &ReleaseManifest,
    policy: VerificationPolicy<'_>,
) -> Result<(), &'static str> {
    if manifest.schema != MANIFEST_SCHEMA {
        return Err("release manifest schema mismatch");
    }
    if manifest.channel != policy.channel {
        return Err("release channel mismatch");
    }
    if manifest.sequence <= policy.current_sequence {
        return Err("release sequence is not newer");
    }

    let version = strict_version(&manifest.version).ok_or("release version malformed")?;
    if manifest.tag != format!("v{}.{}.{}", version.0, version.1, version.2) {
        return Err("release tag mismatch");
    }
    if manifest.architecture != policy.architecture {
        return Err("release architecture mismatch");
    }
    let expected_asset = format!(
        "claudometer-{}-windows-{}.exe",
        manifest.tag, policy.architecture
    );
    if manifest.asset != expected_asset {
        return Err("release asset mismatch");
    }
    if manifest.size == 0 || manifest.size > policy.max_asset_size {
        return Err("release size invalid");
    }
    if !is_lower_hex(&manifest.sha256, 64) {
        return Err("release SHA-256 malformed");
    }
    let minimum = strict_version(&manifest.minimum_updater_version)
        .ok_or("minimum updater version malformed")?;
    if minimum > policy.current_version {
        return Err("updater version below release minimum");
    }

    validate_window(&manifest.issued_at, &manifest.policy_expires_at, policy.now)
}

fn verify_rollback(
    proof: RollbackProof<'_>,
    manifest: &ReleaseManifest,
    policy: VerificationPolicy<'_>,
) -> Result<(), &'static str> {
    verify_signatures(
        proof.bytes,
        proof.signatures,
        &policy.trusted_public_key,
        false,
    )?;
    let authorization: RollbackAuthorization =
        serde_json::from_slice(proof.bytes).map_err(|_| "rollback authorization malformed")?;
    if authorization.schema != ROLLBACK_SCHEMA
        || authorization.channel != policy.channel
        || authorization.from_sequence != policy.current_sequence
        || strict_version(&authorization.from_version) != Some(policy.current_version)
        || authorization.target_sequence != manifest.sequence
        || authorization.target_version != manifest.version
        || authorization.target_tag != manifest.tag
        || authorization.architecture != manifest.architecture
        || authorization.asset != manifest.asset
    {
        return Err("rollback authorization target mismatch");
    }
    validate_window(
        &authorization.issued_at,
        &authorization.expires_at,
        policy.now,
    )
    .map_err(|_| "rollback authorization expired or malformed")
}

fn verify_signatures(
    message: &[u8],
    envelope_bytes: &[u8],
    trusted_public_key: &[u8; 32],
    allow_rotation: bool,
) -> Result<Option<[u8; 32]>, &'static str> {
    let envelope: SignatureEnvelope =
        serde_json::from_slice(envelope_bytes).map_err(|_| "signature envelope malformed")?;
    if envelope.schema != SIGNATURE_SCHEMA
        || envelope.signatures.is_empty()
        || envelope.signatures.len() > MAX_SIGNATURES
    {
        return Err("signature envelope invalid");
    }

    let trusted = VerifyingKey::from_bytes(trusted_public_key)
        .map_err(|_| "trusted release public key invalid")?;
    let trusted_matches = envelope
        .signatures
        .iter()
        .filter(|entry| decode_hex::<32>(&entry.public_key).as_ref() == Some(trusted_public_key))
        .filter(|entry| verify_one(&trusted, message, &entry.signature))
        .count();
    if trusted_matches != 1 {
        return Err("release signature invalid");
    }

    let Some(next_hex) = envelope.next_public_key else {
        if envelope.signatures.len() != 1 {
            return Err("untrusted release signature present");
        }
        return Ok(None);
    };
    if !allow_rotation {
        return Err("key rotation not allowed here");
    }
    let next_bytes = decode_hex::<32>(&next_hex).ok_or("next release public key malformed")?;
    if next_bytes == *trusted_public_key || envelope.signatures.len() != 2 {
        return Err("key rotation envelope invalid");
    }
    let next =
        VerifyingKey::from_bytes(&next_bytes).map_err(|_| "next release public key invalid")?;
    let next_matches = envelope
        .signatures
        .iter()
        .filter(|entry| decode_hex::<32>(&entry.public_key).as_ref() == Some(&next_bytes))
        .filter(|entry| verify_one(&next, message, &entry.signature))
        .count();
    if next_matches != 1 {
        return Err("release key rotation is not cross-signed");
    }
    Ok(Some(next_bytes))
}

fn verify_one(key: &VerifyingKey, message: &[u8], signature_hex: &str) -> bool {
    let Some(bytes) = decode_hex::<64>(signature_hex) else {
        return false;
    };
    key.verify_strict(message, &Signature::from_bytes(&bytes))
        .is_ok()
}

fn validate_window(
    issued_at: &str,
    expires_at: &str,
    now: OffsetDateTime,
) -> Result<(), &'static str> {
    let issued = strict_timestamp(issued_at).ok_or("issued-at timestamp malformed")?;
    let expires = strict_timestamp(expires_at).ok_or("expiry timestamp malformed")?;
    if expires <= issued {
        return Err("release policy window invalid");
    }
    if issued.unix_timestamp() > now.unix_timestamp().saturating_add(CLOCK_SKEW_SECONDS) {
        return Err("release manifest issued in the future");
    }
    if expires <= now {
        return Err("release policy expired");
    }
    Ok(())
}

fn strict_timestamp(value: &str) -> Option<OffsetDateTime> {
    if value.len() != 20 || !value.ends_with('Z') {
        return None;
    }
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

pub fn strict_version(value: &str) -> Option<(u16, u16, u16)> {
    let mut fields = value.split('.');
    let version = (
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    );
    if fields.next().is_some() || value != format!("{}.{}.{}", version.0, version.1, version.2) {
        return None;
    }
    Some(version)
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if !is_lower_hex(value, N * 2) {
        return None;
    }
    let mut decoded = [0u8; N];
    for (index, output) in decoded.iter_mut().enumerate() {
        let high = hex_nibble(value.as_bytes()[index * 2])?;
        let low = hex_nibble(value.as_bytes()[index * 2 + 1])?;
        *output = high << 4 | low;
    }
    Some(decoded)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;
    use time::macros::datetime;

    use super::*;

    const CURRENT: (u16, u16, u16) = (1, 2, 3);
    const NOW: OffsetDateTime = datetime!(2026-09-03 12:00 UTC);

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn manifest(version: &str, sequence: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": MANIFEST_SCHEMA,
            "channel": RELEASE_CHANNEL,
            "sequence": sequence,
            "version": version,
            "tag": format!("v{version}"),
            "issued_at": "2026-09-03T11:00:00Z",
            "policy_expires_at": "2026-09-10T11:00:00Z",
            "architecture": "x64",
            "asset": format!("claudometer-v{version}-windows-x64.exe"),
            "size": 123456,
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "minimum_updater_version": "1.0.0"
        }))
        .unwrap()
    }

    fn signatures(message: &[u8], key: &SigningKey) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": SIGNATURE_SCHEMA,
            "signatures": [{
                "public_key": hex(key.verifying_key().as_bytes()),
                "signature": hex(&key.sign(message).to_bytes())
            }]
        }))
        .unwrap()
    }

    fn policy(key: &SigningKey) -> VerificationPolicy<'static> {
        VerificationPolicy {
            trusted_public_key: *key.verifying_key().as_bytes(),
            channel: RELEASE_CHANNEL,
            architecture: "x64",
            current_sequence: 12,
            current_version: CURRENT,
            now: NOW,
            max_asset_size: 100 * 1024 * 1024,
        }
    }

    #[test]
    fn valid_signature_authenticates_exact_manifest_bytes() {
        let key = signing_key(7);
        let bytes = manifest("1.3.0", 13);
        let signature_bytes = signatures(&bytes, &key);
        let verified = verify(&bytes, &signature_bytes, None, policy(&key)).unwrap();
        assert_eq!(verified.manifest.version, "1.3.0");

        let mut changed = bytes.clone();
        changed.push(b'\n');
        assert_eq!(
            verify(&changed, &signature_bytes, None, policy(&key)),
            Err("release signature invalid")
        );
    }

    #[test]
    fn rejects_wrong_key_and_unsigned_or_malformed_data() {
        let trusted = signing_key(7);
        let wrong = signing_key(8);
        let bytes = manifest("1.3.0", 13);
        assert_eq!(
            verify(&bytes, &signatures(&bytes, &wrong), None, policy(&trusted)),
            Err("release signature invalid")
        );
        assert_eq!(
            verify(&bytes, b"{}", None, policy(&trusted)),
            Err("signature envelope malformed")
        );
        assert_eq!(
            verify(b"{}", &signatures(b"{}", &trusted), None, policy(&trusted)),
            Err("release manifest malformed")
        );
    }

    #[test]
    fn rejects_replay_channel_architecture_expiry_size_hash_and_minimum_updater() {
        let key = signing_key(9);
        let base = manifest("1.3.0", 13);
        let cases = [
            (
                "wrong schema",
                "schema",
                json!("claudometer-release-manifest-v2"),
                "release manifest schema mismatch",
            ),
            (
                "replayed sequence",
                "sequence",
                json!(12),
                "release sequence is not newer",
            ),
            (
                "wrong channel",
                "channel",
                json!("beta"),
                "release channel mismatch",
            ),
            (
                "wrong architecture",
                "architecture",
                json!("arm64"),
                "release architecture mismatch",
            ),
            (
                "noncanonical version",
                "version",
                json!("01.3.0"),
                "release version malformed",
            ),
            ("wrong tag", "tag", json!("v1.3.1"), "release tag mismatch"),
            (
                "wrong asset",
                "asset",
                json!("claudometer-v1.3.0-windows-x64-setup.exe"),
                "release asset mismatch",
            ),
            (
                "expired",
                "policy_expires_at",
                json!("2026-09-03T12:00:00Z"),
                "release policy expired",
            ),
            (
                "malformed issued at",
                "issued_at",
                json!("2026-09-03T11:00:00+00:00"),
                "issued-at timestamp malformed",
            ),
            (
                "future issued at",
                "issued_at",
                json!("2026-09-03T12:06:00Z"),
                "release manifest issued in the future",
            ),
            ("zero size", "size", json!(0), "release size invalid"),
            (
                "uppercase hash",
                "sha256",
                json!("A123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
                "release SHA-256 malformed",
            ),
            (
                "new updater needed",
                "minimum_updater_version",
                json!("1.2.4"),
                "updater version below release minimum",
            ),
        ];
        for (name, field, value, expected) in cases {
            let mut object: serde_json::Value = serde_json::from_slice(&base).unwrap();
            object[field] = value;
            let bytes = serde_json::to_vec(&object).unwrap();
            assert_eq!(
                verify(&bytes, &signatures(&bytes, &key), None, policy(&key)),
                Err(expected),
                "{name}"
            );
        }

        let mut with_unknown: serde_json::Value = serde_json::from_slice(&base).unwrap();
        with_unknown["mirror"] = json!("https://evil.example/update.exe");
        let bytes = serde_json::to_vec(&with_unknown).unwrap();
        assert_eq!(
            verify(&bytes, &signatures(&bytes, &key), None, policy(&key)),
            Err("release manifest malformed")
        );

        let same_version = manifest("1.2.3", 13);
        assert_eq!(
            verify(
                &same_version,
                &signatures(&same_version, &key),
                None,
                policy(&key)
            ),
            Err("release version is not newer")
        );
    }

    #[test]
    fn downgrade_needs_separately_signed_exact_unexpired_authorization() {
        let key = signing_key(10);
        let bytes = manifest("1.1.9", 13);
        let signature_bytes = signatures(&bytes, &key);
        assert_eq!(
            verify(&bytes, &signature_bytes, None, policy(&key)),
            Err("release downgrade not authorized")
        );

        let authorization = serde_json::to_vec(&json!({
            "schema": ROLLBACK_SCHEMA,
            "channel": RELEASE_CHANNEL,
            "from_sequence": 12,
            "from_version": "1.2.3",
            "target_sequence": 13,
            "target_version": "1.1.9",
            "target_tag": "v1.1.9",
            "architecture": "x64",
            "asset": "claudometer-v1.1.9-windows-x64.exe",
            "issued_at": "2026-09-03T11:00:00Z",
            "expires_at": "2026-09-04T11:00:00Z"
        }))
        .unwrap();
        let proof = RollbackProof {
            bytes: &authorization,
            signatures: &signatures(&authorization, &key),
        };
        assert!(verify(&bytes, &signature_bytes, Some(proof), policy(&key)).is_ok());

        let wrong_target = authorization.to_vec();
        let mut value: serde_json::Value = serde_json::from_slice(&wrong_target).unwrap();
        value["target_tag"] = json!("v1.1.8");
        let wrong_target = serde_json::to_vec(&value).unwrap();
        let proof = RollbackProof {
            bytes: &wrong_target,
            signatures: &signatures(&wrong_target, &key),
        };
        assert_eq!(
            verify(&bytes, &signature_bytes, Some(proof), policy(&key)),
            Err("rollback authorization target mismatch")
        );

        let mut value: serde_json::Value = serde_json::from_slice(&authorization).unwrap();
        value["expires_at"] = json!("2026-09-03T12:00:00Z");
        let expired = serde_json::to_vec(&value).unwrap();
        let proof = RollbackProof {
            bytes: &expired,
            signatures: &signatures(&expired, &key),
        };
        assert_eq!(
            verify(&bytes, &signature_bytes, Some(proof), policy(&key)),
            Err("rollback authorization expired or malformed")
        );
    }

    #[test]
    fn rotation_requires_manifest_cross_signature_from_both_keys() {
        let current = signing_key(11);
        let next = signing_key(12);
        let bytes = manifest("1.3.0", 13);
        let envelope = serde_json::to_vec(&json!({
            "schema": SIGNATURE_SCHEMA,
            "signatures": [
                {
                    "public_key": hex(current.verifying_key().as_bytes()),
                    "signature": hex(&current.sign(&bytes).to_bytes())
                },
                {
                    "public_key": hex(next.verifying_key().as_bytes()),
                    "signature": hex(&next.sign(&bytes).to_bytes())
                }
            ],
            "next_public_key": hex(next.verifying_key().as_bytes())
        }))
        .unwrap();
        let verified = verify(&bytes, &envelope, None, policy(&current)).unwrap();
        assert_eq!(
            verified.next_public_key,
            Some(*next.verifying_key().as_bytes())
        );

        let not_cross_signed = serde_json::to_vec(&json!({
            "schema": SIGNATURE_SCHEMA,
            "signatures": [
                {
                    "public_key": hex(current.verifying_key().as_bytes()),
                    "signature": hex(&current.sign(&bytes).to_bytes())
                },
                {
                    "public_key": hex(next.verifying_key().as_bytes()),
                    "signature": "00".repeat(64)
                }
            ],
            "next_public_key": hex(next.verifying_key().as_bytes())
        }))
        .unwrap();
        assert_eq!(
            verify(&bytes, &not_cross_signed, None, policy(&current)),
            Err("release key rotation is not cross-signed")
        );
    }
}
