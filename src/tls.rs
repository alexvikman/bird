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
use std::path::{Path, PathBuf};
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
    cert_pem: String,
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
pub fn generate_ca(tld: &str) -> Result<(String, String)> {
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
    let (cert_pem, key_pem) = generate_ca(tld)?;
    store_ca(&cert_pem, &key_pem, tld)
}

/// Puts back a CA saved before it was replaced (`bird tld` when the
/// privileged step fails).
pub fn restore_ca(cert_pem: &str, key_pem: &str, tld: &str) -> Result<()> {
    store_ca(cert_pem, key_pem, tld)
}

fn store_ca(cert_pem: &str, key_pem: &str, tld: &str) -> Result<()> {
    // The CA is trusted system-wide, so its key must never be readable by others.
    let dir = paths().ca_dir();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;

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
    // PHP must trust the new CA too (and stop trusting the old one).
    write_ca_bundle(tld);
    Ok(())
}

/// A warning in the daemon's log, or on the terminal for CLI commands.
pub(crate) fn warn(msg: &str) {
    if tracing::dispatcher::has_been_set() {
        tracing::warn!("{msg}");
    } else {
        eprintln!("{} {msg}", crate::ui::yellow("warning:"));
    }
}

/// Where systems keep their CA bundle. Bird's static PHP has no default of
/// its own that fits every system.
pub const SYSTEM_CA_BUNDLES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt", // Debian, Ubuntu, Arch
    "/etc/pki/tls/certs/ca-bundle.crt",   // Fedora, RHEL
    "/etc/ssl/ca-bundle.pem",             // openSUSE
    "/etc/ssl/cert.pem",                  // macOS, Alpine
];

/// This system's CA bundle: macOS has one place, Linux distributions each
/// their own.
pub fn system_ca_bundle() -> Option<&'static Path> {
    let candidates = if cfg!(target_os = "macos") {
        &SYSTEM_CA_BUNDLES[3..]
    } else {
        SYSTEM_CA_BUNDLES
    };
    candidates.iter().map(Path::new).find(|path| path.is_file())
}

/// What PHP trusts for outgoing TLS: the system's roots and Bird's CA, so
/// one site can call another over `https://<site>.test`. Bird's CA is only
/// added if `check_ca_for_trust` accepts it (Bird's own, limited to `tld`):
/// PHP must never trust a CA that can vouch for real domains just because it
/// sits in `~/.bird/ca`. Returns the bundle and why the CA was left out, if
/// it was. The roots are bytes: bundles carry comments in any encoding.
fn ca_bundle_contents(
    system: &[u8],
    bird_ca: Option<&str>,
    tld: &str,
) -> (Vec<u8>, Option<String>) {
    let mut bundle = system.trim_ascii_end().to_vec();
    bundle.push(b'\n');
    let left_out = match bird_ca.map(|pem| (pem, check_ca_for_trust(pem, tld))) {
        Some((pem, Ok(name))) => {
            let entry = format!("\n# {name}, added by Bird\n{}\n", pem.trim());
            bundle.extend_from_slice(entry.as_bytes());
            None
        }
        Some((_, Err(e))) => Some(format!("{e:#}")),
        None => None,
    };
    (bundle, left_out)
}

/// Writes `ca-bundle.pem` in `ca_dir` (see `ca_bundle_contents`) and returns
/// the file PHP should use as its CA bundle, or None to set none. Rebuilt
/// every time the CA changes, 00-bird.ini is written and the daemon starts,
/// so a root the system removes or distrusts in an update leaves PHP soon
/// too. Never as root: the bundle (and ~/.bird) must stay the user's.
///
/// Never fails and never leaves PHP worse off than without a bundle: if it
/// can't be built, PHP keeps the bundle it had, or the system's (what it
/// used before Bird made bundles). Without a system bundle PHP keeps its own
/// defaults: one with only Bird's CA would cut it off from every real host.
fn write_ca_bundle_from(ca_dir: &Path, tld: &str, system: Option<&Path>) -> Option<PathBuf> {
    let bundle = ca_dir.join("ca-bundle.pem");
    let system = system?;
    let fallback = || {
        if bundle.is_file() {
            bundle.clone()
        } else {
            system.to_path_buf()
        }
    };
    if crate::sys::is_root() {
        return Some(fallback());
    }
    let roots = match std::fs::read(system) {
        Ok(roots) => roots,
        Err(e) => {
            warn(&format!(
                "could not read {} ({e}); PHP keeps the CA bundle it had",
                system.display()
            ));
            return Some(fallback());
        }
    };
    let ca_cert = ca_dir.join("ca.pem");
    let bird_ca = match std::fs::read_to_string(&ca_cert) {
        Ok(pem) => Some(pem),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn(&format!("could not read {} ({e})", ca_cert.display()));
            None
        }
    };
    let (contents, left_out) = ca_bundle_contents(&roots, bird_ca.as_deref(), tld);
    if let Some(reason) = left_out {
        warn(&format!(
            "PHP does not trust Bird's CA for calls between sites: {reason}"
        ));
    }
    // Usually nothing changed; then there is nothing to write (or to race on).
    if std::fs::read(&bundle).is_ok_and(|current| current == contents) {
        return Some(bundle);
    }
    if let Err(e) = replace_file(ca_dir, &bundle, &contents) {
        warn(&format!(
            "could not write {} ({e:#}); PHP keeps the CA bundle it had",
            bundle.display()
        ));
        return Some(fallback());
    }
    Some(bundle)
}

/// Writes `file` in the private directory `dir` by renaming a finished copy
/// into place, so PHP never reads half a bundle. The copy has a name of this
/// process's own: the daemon and a CLI command may write at the same time.
fn replace_file(dir: &Path, file: &Path, contents: &[u8]) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let written = std::fs::write(&tmp, contents).and_then(|()| std::fs::rename(&tmp, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

pub fn write_ca_bundle_in(ca_dir: &Path, tld: &str) -> Option<PathBuf> {
    write_ca_bundle_from(ca_dir, tld, system_ca_bundle())
}

pub fn write_ca_bundle(tld: &str) -> Option<PathBuf> {
    write_ca_bundle_in(&paths().ca_dir(), tld)
}

pub fn parse_pem(pem: &str) -> Option<CertificateDer<'static>> {
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

/// Checks that `pem` is a CA Bird made for `tld` before it is trusted
/// system-wide, and returns its common name. `bird install` trusts the CA as
/// root from a file in the user's `~/.bird`: whatever sits there must be a CA
/// that can only vouch for `*.<tld>`, never one for every domain.
pub fn check_ca_for_trust(pem: &str, tld: &str) -> Result<String> {
    let der = parse_pem(pem).context("the CA certificate is not a PEM certificate")?;
    let (_, cert) =
        x509_parser::parse_x509_certificate(&der).context("the CA certificate is not valid")?;
    if !cert.is_ca() {
        anyhow::bail!("the CA certificate is not a certificate authority");
    }
    if ca_tld(pem).as_deref() != Some(tld) {
        anyhow::bail!("the CA certificate is not limited to *.{tld}; run `bird install` again");
    }
    let name = common_name(pem).context("the CA certificate has no common name")?;
    if !name.starts_with(CA_NAME_PREFIX) {
        anyhow::bail!("the CA certificate ({name:?}) was not made by Bird");
    }
    Ok(name)
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

    pub fn from_pem(cert: &str, key: &str) -> Result<Self> {
        let key = KeyPair::from_pem(key)?;
        let issuer = Issuer::from_ca_cert_pem(cert, key)?;
        Ok(Self {
            issuer,
            cert_pem: cert.to_string(),
        })
    }

    /// A leaf for `names`; the first one is also the common name.
    fn sign_leaf(&self, names: &[&str]) -> Result<(rcgen::Certificate, KeyPair)> {
        let key = KeyPair::generate()?;
        let mut params =
            CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, names[0]);
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
        let (cert, key) = self.sign_leaf(&[host])?;
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der)?;
        Ok(CertifiedKey::new(vec![cert.der().clone()], signing_key))
    }

    /// A certificate for `domain` and `*.domain` as PEM files: the leaf
    /// followed by the CA (the chain), and the private key.
    pub fn issue_pem(&self, domain: &str) -> Result<(String, String)> {
        let wildcard = format!("*.{domain}");
        let (cert, key) = self.sign_leaf(&[domain, &wildcard])?;
        let chain = format!("{}{}", cert.pem(), self.cert_pem.trim_start());
        Ok((chain, key.serialize_pem()))
    }
}

/// Certificates kept in memory, one per hostname seen.
const MAX_CACHED_CERTS: usize = 2048;

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
        // Any web page can make the browser ask for endless made-up
        // hostnames; start over rather than keep a certificate for each.
        if cache.len() >= MAX_CACHED_CERTS {
            cache.clear();
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

    /// A CA with Bird's name that could vouch for any domain.
    fn unconstrained_ca() -> String {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, format!("{CA_NAME_PREFIX} (x)"));
        params.distinguished_name = dn;
        params.self_signed(&key).unwrap().pem()
    }

    fn certificates(bundle: &[u8]) -> usize {
        String::from_utf8_lossy(bundle)
            .matches("BEGIN CERTIFICATE")
            .count()
    }

    #[test]
    fn ca_bundle_has_system_roots_and_only_birds_ca() {
        let (bird, _) = generate_ca("test").unwrap();
        // A label in Latin-1, as some distributions' bundles have.
        let system =
            b"# Certific\xe9\n-----BEGIN CERTIFICATE-----\nSYSTEM\n-----END CERTIFICATE-----\n\n";
        let (bundle, left_out) = ca_bundle_contents(system, Some(&bird), "test");
        assert!(left_out.is_none());
        assert!(bundle.starts_with(system.trim_ascii_end()));
        assert!(bundle.ends_with(format!("\n{}\n", bird.trim()).as_bytes()));
        assert_eq!(certificates(&bundle), 2);

        // A CA that could vouch for any domain is never added.
        let (bundle, left_out) = ca_bundle_contents(system, Some(&unconstrained_ca()), "test");
        assert_eq!(certificates(&bundle), 1);
        assert!(left_out.is_some());
        // Nor one of Bird's for another TLD.
        let (bundle, left_out) = ca_bundle_contents(system, Some(&bird), "dev");
        assert_eq!(certificates(&bundle), 1);
        assert!(left_out.is_some());
        // No CA yet: the system's roots, and nothing to warn about.
        let (bundle, left_out) = ca_bundle_contents(system, None, "test");
        assert_eq!(certificates(&bundle), 1);
        assert!(left_out.is_none());
    }

    #[test]
    fn writes_the_ca_bundle_and_falls_back_without_breaking_php() {
        if crate::sys::is_root() {
            return; // root never writes the bundle
        }
        let dir = std::env::temp_dir().join(format!("bird-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ca_dir = dir.join("ca");
        std::fs::create_dir_all(&ca_dir).unwrap();
        let system = dir.join("system.pem");
        std::fs::write(
            &system,
            b"# Certific\xe9\n-----BEGIN CERTIFICATE-----\nSYSTEM\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let bundle = ca_dir.join("ca-bundle.pem");
        let read = || std::fs::read(&bundle).unwrap();

        // Before there is a CA: the system's roots, bytes and all.
        assert_eq!(
            write_ca_bundle_from(&ca_dir, "test", Some(&system)),
            Some(bundle.clone())
        );
        assert_eq!(certificates(&read()), 1);
        assert!(read().starts_with(b"# Certific\xe9\n"));

        // Bird's CA for this TLD is added; for another TLD it is taken out.
        let (bird, _) = generate_ca("test").unwrap();
        std::fs::write(ca_dir.join("ca.pem"), &bird).unwrap();
        write_ca_bundle_from(&ca_dir, "test", Some(&system));
        assert_eq!(certificates(&read()), 2);
        assert!(String::from_utf8_lossy(&read()).contains(bird.trim()));
        write_ca_bundle_from(&ca_dir, "dev", Some(&system));
        assert_eq!(certificates(&read()), 1);
        // So is a swapped one that could vouch for any domain.
        std::fs::write(ca_dir.join("ca.pem"), unconstrained_ca()).unwrap();
        write_ca_bundle_from(&ca_dir, "test", Some(&system));
        assert_eq!(certificates(&read()), 1);
        std::fs::write(ca_dir.join("ca.pem"), &bird).unwrap();
        write_ca_bundle_from(&ca_dir, "test", Some(&system));
        let good = read();
        assert_eq!(certificates(&good), 2);

        // No temporary files stay behind, and the directory is private.
        let names: Vec<_> = std::fs::read_dir(&ca_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let mode = std::fs::metadata(&ca_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);

        // System roots that can't be read: PHP keeps the bundle it had,
        // never one with Bird's CA alone.
        let gone = dir.join("gone.pem");
        assert_eq!(
            write_ca_bundle_from(&ca_dir, "test", Some(&gone)),
            Some(bundle.clone())
        );
        assert_eq!(read(), good);
        // Without an earlier bundle it is pointed at the system's, as before.
        let empty = dir.join("empty-ca");
        assert_eq!(
            write_ca_bundle_from(&empty, "test", Some(&gone)),
            Some(gone.clone())
        );
        // A bundle that can't be written (its directory is a file) likewise.
        std::fs::write(dir.join("file"), "").unwrap();
        assert_eq!(
            write_ca_bundle_from(&dir.join("file"), "test", Some(&system)),
            Some(system.clone())
        );

        // No system bundle at all: PHP keeps its own defaults, and what is
        // there (00-bird.ini may still name it) is left alone.
        assert_eq!(write_ca_bundle_from(&ca_dir, "test", None), None);
        assert_eq!(read(), good);
        assert!(!empty.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn ca_only_vouches_for_its_tld() {
        let (cert_pem, key_pem) = generate_ca("test").unwrap();
        assert_eq!(ca_tld(&cert_pem).as_deref(), Some("test"));
        assert_eq!(sha1_fingerprint(&cert_pem).unwrap().len(), 40);
        let ca = Ca::from_pem(&cert_pem, &key_pem).unwrap();

        for host in ["app.test", "www.api.app.test"] {
            let (leaf, _) = ca.sign_leaf(&[host]).unwrap();
            verify(&cert_pem, leaf.der(), host).unwrap_or_else(|e| panic!("{host}: {e:?}"));
        }
        for host in ["example.com", "test.example.com", "nottest"] {
            let (leaf, _) = ca.sign_leaf(&[host]).unwrap();
            assert!(
                verify(&cert_pem, leaf.der(), host).is_err(),
                "{host} must be rejected"
            );
        }
        let (leaf, _) = ca.sign_leaf(&["127.0.0.1"]).unwrap();
        assert!(
            verify(&cert_pem, leaf.der(), "127.0.0.1").is_err(),
            "IP certs must be rejected"
        );
    }

    #[test]
    fn only_birds_own_constrained_ca_is_trusted() {
        let (cert_pem, key_pem) = generate_ca("test").unwrap();
        let name = check_ca_for_trust(&cert_pem, "test").unwrap();
        assert!(name.starts_with(CA_NAME_PREFIX));
        // A CA for another TLD (config.toml says `test`).
        assert!(check_ca_for_trust(&generate_ca("com").unwrap().0, "test").is_err());

        // An unconstrained CA, as someone else could put in ~/.bird/ca.
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, format!("{CA_NAME_PREFIX} (x)"));
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let unconstrained = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
        assert!(check_ca_for_trust(&unconstrained.pem(), "test").is_err());

        // A leaf signed by the real CA is not a CA; garbage is nothing.
        let ca = Ca::from_pem(&cert_pem, &key_pem).unwrap();
        let (leaf, _) = ca.sign_leaf(&["app.test"]).unwrap();
        assert!(check_ca_for_trust(&leaf.pem(), "test").is_err());
        assert!(check_ca_for_trust("root:*:0:0::/root:/bin/sh\n", "test").is_err());
    }

    #[test]
    fn pem_files_cover_the_domain_and_its_subdomains() {
        let (ca_pem, ca_key) = generate_ca("test").unwrap();
        let ca = Ca::from_pem(&ca_pem, &ca_key).unwrap();
        let (chain, key) = ca.issue_pem("app.test").unwrap();
        let certs: Vec<_> = CertificateDer::pem_slice_iter(chain.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(certs.len(), 2, "leaf and CA");
        assert_eq!(certs[1], parse_pem(&ca_pem).unwrap());
        for host in ["app.test", "www.app.test"] {
            verify(&ca_pem, &certs[0], host).unwrap_or_else(|e| panic!("{host}: {e:?}"));
        }
        assert!(verify(&ca_pem, &certs[0], "other.test").is_err());
        assert!(KeyPair::from_pem(&key).is_ok());
    }
}
