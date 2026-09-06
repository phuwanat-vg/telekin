//! QUIC transport setup for Telekin.
//!
//! * The host keeps a self-signed certificate on disk and prints its SHA-256
//!   fingerprint. Identity is *key-pinning* based (like SSH), not CA based:
//!   the viewer either pins the expected fingerprint or accepts on first use.
//!   The certificate persists across restarts, so a fleet is pinned once
//!   rather than re-pinned after every reboot.
//! * BBR congestion control is enabled on both sides — it holds throughput
//!   much better than CUBIC on the lossy WiFi links robots live on.
//! * ALPN is `tk/1`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

/// Re-exported so applications use exactly the QUIC version this crate was
/// built against — the endpoint and connection types cross the boundary.
pub use quinn;

pub mod discovery;

pub const ALPN: &[u8] = b"tk/1";

/// Idle timeout before a dead peer is dropped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.max_idle_timeout(Some(IDLE_TIMEOUT.try_into().expect("valid idle timeout")));
    t.keep_alive_interval(Some(Duration::from_secs(2)));
    // One uni stream per video frame; allow plenty in flight.
    t.max_concurrent_uni_streams(1024u32.into());
    t.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    t
}

/// Hex fingerprint like `ab:cd:...` of a DER certificate.
pub fn fingerprint(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Parse a fingerprint in the format produced by [`fingerprint`] (colons and
/// case are ignored).
pub fn parse_fingerprint(s: &str) -> anyhow::Result<[u8; 32]> {
    let hex: String = s.chars().filter(|c| *c != ':').collect();
    anyhow::ensure!(hex.len() == 64, "fingerprint must be 32 hex bytes");
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk)?;
        out[i] = u8::from_str_radix(s, 16).context("invalid hex in fingerprint")?;
    }
    Ok(out)
}

/// Load the host's persistent identity, creating it on first run.
///
/// The fingerprint is what authenticates a host, so it has to survive a
/// reboot: regenerating it each start would mean re-pinning every robot in the
/// fleet every morning. The key is written with owner-only permissions.
fn load_or_create_identity(
    dir: &std::path::Path,
) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let cert_path = dir.join("host-cert.der");
    let key_path = dir.join("host-key.der");

    if cert_path.exists() && key_path.exists() {
        let cert = std::fs::read(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key = std::fs::read(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        return Ok((
            CertificateDer::from(cert),
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        ));
    }

    let generated = rcgen::generate_simple_self_signed(vec!["chassis".into()])?;
    let cert_der = generated.cert.der().to_vec();
    let key_der = generated.key_pair.serialize_der();

    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&cert_path, &cert_der)?;
    write_private(&key_path, &key_der)?;
    tracing::info!("created a new host identity in {}", dir.display());

    Ok((
        CertificateDer::from(cert_der),
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
    ))
}

/// Write a file only the owner can read. Permissions are advisory on Windows,
/// where the user profile directory already restricts access.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Where a host keeps its identity when no explicit directory is given.
pub fn default_identity_dir() -> std::path::PathBuf {
    let base = config_base();
    let dir = base.join("telekin");

    // This directory holds the key behind the fingerprint viewers pin, so
    // renaming the product must not quietly hand every robot a new identity.
    // Carry the old directory over the first time, and only when there is
    // nothing at the new path to overwrite.
    let old = base.join("wasp-remote");
    if !dir.exists() && old.is_dir() {
        match std::fs::rename(&old, &dir) {
            Ok(()) => tracing::info!(
                "moved this host's identity from {} to {}; the fingerprint is unchanged",
                old.display(),
                dir.display()
            ),
            Err(e) => tracing::warn!(
                "could not move {} to {} ({e}); a new identity will be created, so viewers \
                 pinning the old fingerprint will need the new one",
                old.display(),
                dir.display()
            ),
        }
    }
    dir
}

fn config_base() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Build a listening endpoint using the host's persistent identity, creating
/// one on first run. Returns the endpoint and the certificate's SHA-256
/// fingerprint, which is stable across restarts.
pub fn server_endpoint(
    listen: std::net::SocketAddr,
    identity_dir: &std::path::Path,
) -> anyhow::Result<(quinn::Endpoint, String)> {
    let (cert_der, key) = load_or_create_identity(identity_dir)?;
    let fp = fingerprint(cert_der.as_ref());

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let mut server_config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(Arc::new(tls))?));
    server_config.transport_config(Arc::new(transport_config()));

    let endpoint = quinn::Endpoint::server(server_config, listen)?;
    Ok((endpoint, fp))
}

/// Build a client endpoint. If `pinned` is set, the connection fails unless
/// the host presents a certificate with that SHA-256 fingerprint.
pub fn client_endpoint(pinned: Option<[u8; 32]>) -> anyhow::Result<quinn::Endpoint> {
    let verifier = PinnedVerifier {
        expected: pinned,
        provider: rustls::crypto::ring::default_provider().into(),
    };
    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let mut client_config =
        quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(Arc::new(tls))?));
    client_config.transport_config(Arc::new(transport_config()));

    let mut endpoint = quinn::Endpoint::client((std::net::Ipv6Addr::UNSPECIFIED, 0).into())
        .or_else(|_| quinn::Endpoint::client((std::net::Ipv4Addr::UNSPECIFIED, 0).into()))?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

/// Accepts self-signed certificates, optionally requiring a pinned
/// SHA-256 fingerprint. Signatures are still fully verified, so a
/// pinned connection authenticates the host end-to-end.
#[derive(Debug)]
struct PinnedVerifier {
    expected: Option<[u8; 32]>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if let Some(expected) = &self.expected {
            let actual = Sha256::digest(end_entity.as_ref());
            if actual.as_slice() != expected {
                return Err(rustls::Error::General(format!(
                    "host certificate fingerprint mismatch (got {})",
                    fingerprint(end_entity.as_ref())
                )));
            }
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
