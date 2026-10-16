//! Local certificate authority and on-the-fly certificates.
//!
//! Bird keeps one CA (trusted in the system keychain at install time) and
//! mints a leaf certificate the first time a hostname is requested over TLS,
//! so every `*.test` site works over HTTPS without per-site setup. The CA is
//! name-constrained to the configured TLD, so even its key cannot be used to
//! impersonate real domains.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use time::{Duration, OffsetDateTime};

use crate::daemon::State;
use crate::paths::{paths, user_home};

pub const CA_NAME_PREFIX: &str = "Bird Development CA";

pub struct Ca {
    issuer: Issuer<'static, KeyPair>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaStatus {
    Existing,
    Created,
    /// An old CA (no name constraints, or another TLD) was replaced; the new
    /// one must be trusted again and the daemon restarted.
    Replaced,
}

/// Makes sure a CA limited to `*.<tld>` exists, replacing one that isn't.
pub fn ensure_ca(tld: &str) -> Result<CaStatus> {
    let existed = paths().ca_cert().is_file() && paths().ca_key().is_file();
    if existed && installed_ca_tld().as_deref() == Some(tld) {
        return Ok(CaStatus::Existing);
    }
    write_ca(tld)?;
    Ok(if existed {
        CaStatus::Replaced
    } else {
        CaStatus::Created
    })
}

/// Creates a CA only if there is none at all (the daemon never replaces a
/// trusted CA on its own; `bird install` and `bird tld` do).
pub fn ensure_ca_exists(tld: &str) -> Result<()> {
    if !(paths().ca_cert().is_file() && paths().ca_key().is_file()) {
        write_ca(tld)?;
    }
    Ok(())
}

/// A self-signed CA that can only issue certificates for `<tld>` and its
/// subdomains, and none for IP addresses. The system trusts this CA, so a
/// leaked key must not be usable to impersonate real domains.
fn generate_ca(tld: &str) -> Result<(String, String)> {
    let user = user_home()
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("{CA_NAME_PREFIX} ({user})"));
    dn.push(DnType::OrganizationName, "Bird");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    // RFC 5280: a dNSName constraint `test` matches `test` and every `*.test`.
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![GeneralSubtree::DnsName(tld.to_string())],
        excluded_subtrees: vec![
            GeneralSubtree::IpAddress(CidrSubnet::V4([0; 4], [0; 4])),
            GeneralSubtree::IpAddress(CidrSubnet::V6([0; 16], [0; 16])),
        ],
    });
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(3650);
    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    Ok((cert.pem(), key.serialize_pem()))
}

fn write_ca(tld: &str) -> Result<()> {
    // The CA is trusted system-wide, so its key must never be readable by others.
    let dir = paths().ca_dir();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let (cert_pem, key_pem) = generate_ca(tld)?;

    // Create the key file as 0600 from the start (no window with umask
    // permissions), then move it into place.
    let tmp_key = paths().ca_key().with_extension("pem.tmp");
    let _ = std::fs::remove_file(&tmp_key);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp_key)?;
    file.write_all(key_pem.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp_key, paths().ca_key())?;
    let tmp_cert = paths().ca_cert().with_extension("pem.tmp");
    std::fs::write(&tmp_cert, cert_pem)?;
    std::fs::rename(&tmp_cert, paths().ca_cert())?;
    Ok(())
}

fn parse_pem(pem: &str) -> Option<CertificateDer<'static>> {
    CertificateDer::from_pem_slice(pem.as_bytes()).ok()
}

/// The TLD a CA certificate is limited to, if it has exactly that one constraint.
pub fn ca_tld(pem: &str) -> Option<String> {
    let der = parse_pem(pem)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der).ok()?;
    let constraints = cert.name_constraints().ok()??;
    let permitted = constraints.value.permitted_subtrees.as_ref()?;
    match permitted.as_slice() {
        [subtree] => match subtree.base {
            x509_parser::extensions::GeneralName::DNSName(name) => Some(name.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn installed_ca_tld() -> Option<String> {
    ca_tld(&std::fs::read_to_string(paths().ca_cert()).ok()?)
}

pub fn common_name(pem: &str) -> Option<String> {
    let der = parse_pem(pem)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der).ok()?;
    let cn = cert.subject().iter_common_name().next()?.as_str().ok()?;
    Some(cn.to_string())
}

/// Common name of the installed CA, used to find it in the keychain.
pub fn ca_common_name() -> Option<String> {
    common_name(&std::fs::read_to_string(paths().ca_cert()).ok()?)
}

/// SHA-1 fingerprint (uppercase hex), the way `security` identifies certificates.
pub fn sha1_fingerprint(pem: &str) -> Option<String> {
    let der = parse_pem(pem)?;
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &der);
    Some(digest.as_ref().iter().map(|b| format!("{b:02X}")).collect())
}

impl Ca {
    pub fn load() -> Result<Self> {
        let cert = std::fs::read_to_string(paths().ca_cert())
            .context("CA certificate missing; run `bird install`")?;
        Self::from_pem(&cert, &std::fs::read_to_string(paths().ca_key())?)
    }

    fn from_pem(cert: &str, key: &str) -> Result<Self> {
        let key = KeyPair::from_pem(key)?;
        let issuer = Issuer::from_ca_cert_pem(cert, key)?;
        Ok(Self { issuer })
    }

    fn sign_leaf(&self, host: &str) -> Result<(rcgen::Certificate, KeyPair)> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::new(vec![host.to_string()])?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);
        params.distinguished_name = dn;
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(1);
        // Apple rejects TLS server certificates valid for more than 398 days.
        params.not_after = now + Duration::days(397);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        let cert = params.signed_by(&key, &self.issuer)?;
        Ok((cert, key))
    }

    pub fn issue(&self, host: &str) -> Result<CertifiedKey> {
        let (cert, key) = self.sign_leaf(host)?;
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der)?;
        Ok(CertifiedKey::new(vec![cert.der().clone()], signing_key))
    }
}

pub struct CertResolver {
    ca: Ca,
    state: Arc<State>,
    cache: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CertResolver")
    }
}

impl CertResolver {
    pub fn new(ca: Ca, state: Arc<State>) -> Self {
        Self {
            ca,
            state,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let host = hello.server_name()?.to_ascii_lowercase();
        let tld = self.state.config().tld.clone();
        if !host.ends_with(&format!(".{tld}")) {
            return None;
        }
        let mut cache = self.cache.lock().unwrap();
        if let Some(key) = cache.get(&host) {
            return Some(key.clone());
        }
        match self.ca.issue(&host) {
            Ok(key) => {
                let key = Arc::new(key);
                cache.insert(host, key.clone());
                Some(key)
            }
            Err(e) => {
                tracing::error!("could not issue certificate for {host}: {e:#}");
                None
            }
        }
    }
}

pub fn server_config(resolver: CertResolver) -> Result<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(resolver));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{ServerName, UnixTime};

    fn verify(ca_pem: &str, leaf: &CertificateDer<'_>, host: &str) -> Result<(), webpki::Error> {
        let ca_der = parse_pem(ca_pem).unwrap();
        let anchor = webpki::anchor_from_trusted_cert(&ca_der).unwrap();
        let ee = webpki::EndEntityCert::try_from(leaf).unwrap();
        ee.verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &[anchor],
            &[],
            UnixTime::now(),
            webpki::KeyUsage::server_auth(),
            None,
            None,
        )?;
        ee.verify_is_valid_for_subject_name(&ServerName::try_from(host.to_string()).unwrap())
    }

    #[test]
    fn ca_only_vouches_for_its_tld() {
        let (cert_pem, key_pem) = generate_ca("test").unwrap();
        assert_eq!(ca_tld(&cert_pem).as_deref(), Some("test"));
        assert_eq!(sha1_fingerprint(&cert_pem).unwrap().len(), 40);
        let ca = Ca::from_pem(&cert_pem, &key_pem).unwrap();

        for host in ["app.test", "www.api.app.test"] {
            let (leaf, _) = ca.sign_leaf(host).unwrap();
            verify(&cert_pem, leaf.der(), host).unwrap_or_else(|e| panic!("{host}: {e:?}"));
        }
        for host in ["example.com", "test.example.com", "nottest"] {
            let (leaf, _) = ca.sign_leaf(host).unwrap();
            assert!(
                verify(&cert_pem, leaf.der(), host).is_err(),
                "{host} must be rejected"
            );
        }
        let (leaf, _) = ca.sign_leaf("127.0.0.1").unwrap();
        assert!(
            verify(&cert_pem, leaf.der(), "127.0.0.1").is_err(),
            "IP certs must be rejected"
        );
    }
}
