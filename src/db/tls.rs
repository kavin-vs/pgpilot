use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Whether/how to use TLS for a connection. Derived either from a saved
/// `Profile`'s ssl fields or from the `--ssl*` CLI flags — see
/// `main.rs::resolve_conninfo`. `--dsn` connections stay `Disabled`
/// regardless (a documented limitation: we don't parse sslmode out of an
/// arbitrary user-supplied DSN string).
#[derive(Debug, Clone, Default)]
pub enum TlsMode {
    #[default]
    Disabled,
    Enabled {
        /// CA cert to verify the server against. If absent, we accept any
        /// server certificate (encrypt-only, no verification) — the chosen
        /// default for a bare `ssl = true` with no CA configured.
        root_cert: Option<String>,
        /// Client cert + key pair, for mutual TLS. Both or neither.
        client_cert: Option<String>,
        client_key: Option<String>,
    },
}

impl TlsMode {
    pub fn from_parts(
        ssl: bool,
        root_cert: Option<String>,
        client_cert: Option<String>,
        client_key: Option<String>,
    ) -> Self {
        if ssl {
            TlsMode::Enabled {
                root_cert,
                client_cert,
                client_key,
            }
        } else {
            TlsMode::Disabled
        }
    }
}

/// Accepts any server certificate: used when SSL is enabled but no CA root
/// cert was configured to verify against (encrypt-only, matching sslmode's
/// `require` rather than `verify-full`).
#[derive(Debug)]
struct AcceptAnyServerCert;

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path).with_context(|| format!("failed to open {path}"))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse certificate(s) from {path}"))
}

fn load_private_key(path: &str) -> Result<PrivateKeyDer<'static>> {
    let file = File::open(path).with_context(|| format!("failed to open {path}"))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("failed to parse private key from {path}"))?
        .with_context(|| format!("no private key found in {path}"))
}

/// Builds the rustls-backed TLS connector for `tokio_postgres::connect`.
/// Only called when `TlsMode::Enabled`.
pub fn make_connector(
    root_cert: Option<&str>,
    client_cert: Option<&str>,
    client_key: Option<&str>,
) -> Result<MakeRustlsConnect> {
    let builder = ClientConfig::builder();

    let builder = match root_cert {
        Some(path) => {
            let mut root_store = RootCertStore::empty();
            for cert in load_certs(path)? {
                root_store
                    .add(cert)
                    .with_context(|| format!("invalid CA certificate in {path}"))?;
            }
            builder.with_root_certificates(root_store)
        }
        None => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert)),
    };

    let config = match (client_cert, client_key) {
        (Some(cert_path), Some(key_path)) => {
            let certs = load_certs(cert_path)?;
            let key = load_private_key(key_path)?;
            builder
                .with_client_auth_cert(certs, key)
                .context("invalid client certificate/key pair")?
        }
        (None, None) => builder.with_no_client_auth(),
        _ => anyhow::bail!("SSL client cert and client key must both be set, or neither"),
    };

    Ok(MakeRustlsConnect::new(config))
}
