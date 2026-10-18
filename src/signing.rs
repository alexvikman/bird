//! Verifies signatures made with `ssh-keygen -Y sign` (OpenSSH's SSHSIG
//! format, Ed25519 keys), so Bird only trusts a release's SHA256SUMS
//! (`bird self-update`) or the PHP builds' manifest.json (`bird php:install`)
//! if it was signed with a key compiled into Bird.
//!
//! The private keys live in GitHub environments that need the owner's
//! approval; someone who can push to the repository or edit a release can't
//! produce a valid signature.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Namespace the release signatures are made in (`ssh-keygen -n`).
pub const NAMESPACE: &str = "bird-release";

/// Public keys whose signatures Bird accepts for its releases, as in an
/// OpenSSH `.pub` file. More than one allows rotating keys.
pub const RELEASE_KEYS: &[&str] = &[
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII5iuep2RhMNGoxLgz+sc8qaXQ9as6/ZcDCzVpJPXVyn bird-release",
];

/// Namespace the PHP builds' manifest.json is signed in.
pub const PHP_NAMESPACE: &str = "bird-php";

/// Public keys whose signatures Bird accepts for the PHP builds' manifest.
/// A separate key from the release key: it signs in the `php-release`
/// environment (.github/workflows/php-builds.yml), and can't sign releases.
pub const PHP_KEYS: &[&str] =
    &["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOFCBm+uCYPaByT8wvmM5u+Fw9t0f302T2/bJ1FXGsbp bird-php"];

/// Reads an SSH wire-format `string` (u32 length + bytes).
fn take<'a>(data: &mut &'a [u8]) -> Result<&'a [u8]> {
    if data.len() < 4 {
        bail!("truncated signature");
    }
    let len = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
    let rest = &data[4..];
    if rest.len() < len {
        bail!("truncated signature");
    }
    let (value, tail) = rest.split_at(len);
    *data = tail;
    Ok(value)
}

fn put(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

/// The raw 32-byte Ed25519 key from an OpenSSH public key line.
fn ed25519_key_from_line(line: &str) -> Result<Vec<u8>> {
    let mut parts = line.split_whitespace();
    let (Some("ssh-ed25519"), Some(b64)) = (parts.next(), parts.next()) else {
        bail!("not an ssh-ed25519 public key");
    };
    let blob = STANDARD.decode(b64).context("bad base64 in public key")?;
    ed25519_key_from_blob(&blob)
}

fn ed25519_key_from_blob(blob: &[u8]) -> Result<Vec<u8>> {
    let mut data = blob;
    if take(&mut data)? != b"ssh-ed25519" {
        bail!("only ssh-ed25519 keys are supported");
    }
    let key = take(&mut data)?;
    if key.len() != 32 {
        bail!("bad Ed25519 key length");
    }
    Ok(key.to_vec())
}

/// Checks an armored SSH signature over `message` against `allowed` keys.
pub fn verify(message: &[u8], armored: &str, namespace: &str, allowed: &[&str]) -> Result<()> {
    if allowed.is_empty() {
        bail!("this build of Bird has no {namespace} signing key, so it can't verify signatures");
    }
    let b64: String = armored
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("-----"))
        .collect();
    let blob = STANDARD
        .decode(b64)
        .context("signature is not valid base64")?;

    // PROTOCOL.sshsig: "SSHSIG" u32(version) string(publickey)
    // string(namespace) string(reserved) string(hash_algorithm) string(signature)
    let Some(mut data) = blob.strip_prefix(b"SSHSIG") else {
        bail!("not an SSH signature");
    };
    if data.len() < 4 || u32::from_be_bytes(data[..4].try_into().unwrap()) != 1 {
        bail!("unsupported SSH signature version");
    }
    data = &data[4..];
    let public_key = ed25519_key_from_blob(take(&mut data)?)?;
    let sig_namespace = take(&mut data)?;
    let reserved = take(&mut data)?;
    let hash_alg = take(&mut data)?;
    let mut signature = take(&mut data)?;

    if sig_namespace != namespace.as_bytes() {
        bail!(
            "signature is for namespace {:?}, expected {namespace:?}",
            String::from_utf8_lossy(sig_namespace)
        );
    }
    let trusted = allowed
        .iter()
        .filter_map(|line| ed25519_key_from_line(line).ok())
        .any(|k| k == public_key);
    if !trusted {
        bail!("signed with a key Bird does not trust");
    }
    if take(&mut signature)? != b"ssh-ed25519" {
        bail!("only ssh-ed25519 signatures are supported");
    }
    let raw_sig = take(&mut signature)?;

    let digest = match hash_alg {
        b"sha512" => ring::digest::digest(&ring::digest::SHA512, message),
        b"sha256" => ring::digest::digest(&ring::digest::SHA256, message),
        other => bail!(
            "unsupported hash algorithm {:?}",
            String::from_utf8_lossy(other)
        ),
    };
    let mut signed = Vec::with_capacity(128);
    signed.extend_from_slice(b"SSHSIG");
    put(&mut signed, namespace.as_bytes());
    put(&mut signed, reserved);
    put(&mut signed, hash_alg);
    put(&mut signed, digest.as_ref());

    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public_key)
        .verify(&signed, raw_sig)
        .map_err(|_| anyhow::anyhow!("signature does not match"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only key and signature (`ssh-keygen -Y sign -n bird-release`);
    // not a release key.
    const TEST_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGdRw8jMA3sRIl6q7LHYBrRRF1OvXK2cfp/z4vuxqov4 bird-test";
    const MESSAGE: &[u8] = b"abc123  bird-0.4.0-aarch64-apple-darwin.tar.gz\n";
    const SIGNATURE: &str = "-----BEGIN SSH SIGNATURE-----
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgZ1HDyMwDexEiXqrssdgGtFEXU6
9crZx+n/Pi+7Gqi/gAAAAMYmlyZC1yZWxlYXNlAAAAAAAAAAZzaGE1MTIAAABTAAAAC3Nz
aC1lZDI1NTE5AAAAQGcaBuTx2FarbdGnc2SPmd7YRw4VErEyQsr7U15/LEJiXgleH2lPE5
Nyf76+TkKmEs79iwnRfuvkIV7P5JQJzQ8=
-----END SSH SIGNATURE-----
";
    // Another key, for "untrusted signer".
    const OTHER_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIH8mh6Qo7gUkX81IDd2lP1dO7RbolUitz66cgzTZzaV other";

    #[test]
    fn accepts_a_valid_signature() {
        verify(MESSAGE, SIGNATURE, NAMESPACE, &[TEST_KEY]).unwrap();
        verify(MESSAGE, SIGNATURE, NAMESPACE, &[OTHER_KEY, TEST_KEY]).unwrap();
    }

    #[test]
    fn rejects_tampering_and_wrong_keys() {
        let tampered = b"ffffff  bird-0.4.0-aarch64-apple-darwin.tar.gz\n";
        assert!(
            verify(tampered, SIGNATURE, NAMESPACE, &[TEST_KEY]).is_err(),
            "changed message"
        );
        assert!(
            verify(MESSAGE, SIGNATURE, NAMESPACE, &[OTHER_KEY]).is_err(),
            "untrusted key"
        );
        assert!(
            verify(MESSAGE, SIGNATURE, "file", &[TEST_KEY]).is_err(),
            "other namespace"
        );
        assert!(
            verify(MESSAGE, SIGNATURE, NAMESPACE, &[]).is_err(),
            "no keys compiled in"
        );
        assert!(verify(MESSAGE, "garbage", NAMESPACE, &[TEST_KEY]).is_err());
    }

    #[test]
    fn release_keys_parse() {
        for key in RELEASE_KEYS.iter().chain(PHP_KEYS) {
            ed25519_key_from_line(key).unwrap();
        }
        assert!(!PHP_KEYS.is_empty(), "no PHP manifest key");
        // The PHP key lives in another environment (`php-release`, main)
        // than the release key (`release`, v* tags); one key never signs both.
        for key in PHP_KEYS {
            let key = ed25519_key_from_line(key).unwrap();
            assert!(
                RELEASE_KEYS
                    .iter()
                    .all(|r| ed25519_key_from_line(r).unwrap() != key),
                "a PHP key is also a release key"
            );
        }
    }
}
