//! TLS / mTLS helpers for the remote-signer daemon transport (rustls, aws-lc-rs provider).
//!
//! The daemon presents a server certificate the node verifies against a pinned CA. When a client CA
//! is configured on the daemon (and a client identity on the node), the link is mutually authenticated
//! (mTLS). The node verifies the daemon's cert SAN against [`SERVER_NAME`].

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};

/// SAN the daemon certificate must carry and the node verifies.
pub(crate) const SERVER_NAME: &str = "rln-remote-signer";

/// Install the aws-lc-rs crypto provider as the process default if none is set yet (idempotent).
fn ensure_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

pub(crate) fn load_certs(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let mut reader = BufReader::new(
        File::open(path).with_context(|| format!("open certificate {}", path.display()))?,
    );
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parse certificates in {}", path.display()))?;
    anyhow::ensure!(!certs.is_empty(), "no certificates in {}", path.display());
    Ok(certs)
}

pub(crate) fn load_key(path: &Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    let mut reader = BufReader::new(
        File::open(path).with_context(|| format!("open private key {}", path.display()))?,
    );
    rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("parse private key in {}", path.display()))?
        .ok_or_else(|| anyhow!("no private key in {}", path.display()))
}

fn root_store(ca_path: &Path) -> anyhow::Result<RootCertStore> {
    let mut store = RootCertStore::empty();
    for cert in load_certs(ca_path)? {
        store
            .add(cert)
            .context("add CA certificate to root store")?;
    }
    Ok(store)
}

/// Build the daemon's server config. If `client_ca` is `Some`, require and verify client certs (mTLS).
pub(crate) fn server_config(
    cert_path: &Path,
    key_path: &Path,
    client_ca: Option<&Path>,
) -> anyhow::Result<Arc<ServerConfig>> {
    ensure_crypto_provider();
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    let config = match client_ca {
        Some(ca) => {
            let roots = Arc::new(root_store(ca)?);
            let verifier = rustls::server::WebPkiClientVerifier::builder(roots)
                .build()
                .context("build client certificate verifier")?;
            ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(certs, key)
                .context("server config with client auth")?
        }
        None => ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("server config")?,
    };
    Ok(Arc::new(config))
}

/// Build the node's client config. Verifies the daemon cert against `ca_path`. If `client_identity`
/// is `Some`, present a client certificate (mTLS).
pub(crate) fn client_config(
    ca_path: &Path,
    client_identity: Option<(&Path, &Path)>,
) -> anyhow::Result<Arc<ClientConfig>> {
    ensure_crypto_provider();
    let roots = root_store(ca_path)?;
    let builder = ClientConfig::builder().with_root_certificates(roots);
    let config = match client_identity {
        Some((cert, key)) => builder
            .with_client_auth_cert(load_certs(cert)?, load_key(key)?)
            .context("client config with client auth")?,
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

/// Discover node-side TLS material by convention under `<dir>/remote-signer-tls/`:
/// - `ca.pem` present → verify the daemon cert against it (TLS). Absent → plaintext (`Ok(None)`).
/// - `client.pem` + `client.key` present → also present a client cert (mTLS).
pub(crate) fn node_client_config_from_dir(
    storage_dir: &Path,
) -> anyhow::Result<Option<Arc<ClientConfig>>> {
    let dir = storage_dir.join("remote-signer-tls");
    let ca = dir.join("ca.pem");
    if !ca.exists() {
        return Ok(None);
    }
    let client_cert = dir.join("client.pem");
    let client_key = dir.join("client.key");
    let identity = if client_cert.exists() && client_key.exists() {
        Some((client_cert.clone(), client_key.clone()))
    } else {
        None
    };
    let identity_ref = identity.as_ref().map(|(c, k)| (c.as_path(), k.as_path()));
    Ok(Some(client_config(&ca, identity_ref)?))
}
