//! Local certificate authority and on-the-fly certificates.
//!
//! Bird keeps one CA (trusted in the system keychain at install time) and
//! mints a leaf certificate the first time a hostname is requested over TLS,
//! so every `*.test` site works over HTTPS without per-site setup.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use time::{Duration, OffsetDateTime};

use crate::daemon::State;
use crate::paths::{paths, user_home};

pub const CA_NAME_PREFIX: &str = "Bird Development CA";

pub struct Ca {
    issuer: Issuer<'static, KeyPair>,
}

/// Creates the CA if it does not exist yet. Returns `true` when a new one was made.
pub fn ensure_ca() -> Result<bool> {
    if paths().ca_cert().is_file() && paths().ca_key().is_file() {
        return Ok(false);
    }
    // The CA is trusted system-wide, so its key must never be readable by others.
    let dir = paths().ca_dir();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let user = user_home()
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("{CA_NAME_PREFIX} ({user})"));
    dn.push(DnType::OrganizationName, "Bird");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(3650);

    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    // Create the key file as 0600 from the start (no window with umask
    // permissions), then move it into place.
    let tmp_key = paths().ca_key().with_extension("pem.tmp");
    let _ = std::fs::remove_file(&tmp_key);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp_key)?;
    file.write_all(key.serialize_pem().as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp_key, paths().ca_key())?;
    std::fs::write(paths().ca_cert(), cert.pem())?;
    Ok(true)
}

/// Common name of the installed CA, used to find it in the keychain.
pub fn ca_common_name() -> Option<String> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    let der = CertificateDer::from_pem_file(paths().ca_cert()).ok()?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der).ok()?;
    let cn = cert.subject().iter_common_name().next()?.as_str().ok()?;
    Some(cn.to_string())
}

impl Ca {
    pub fn load() -> Result<Self> {
        let cert = std::fs::read_to_string(paths().ca_cert())
            .context("CA certificate missing; run `bird install`")?;
        let key = KeyPair::from_pem(&std::fs::read_to_string(paths().ca_key())?)?;
        let issuer = Issuer::from_ca_cert_pem(&cert, key)?;
        Ok(Self { issuer })
    }

    pub fn issue(&self, host: &str) -> Result<CertifiedKey> {
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
