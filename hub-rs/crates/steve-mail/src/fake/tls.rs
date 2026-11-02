//! Throwaway certificate authority for the fake servers.
//!
//! Each [`TestCa`] is generated fresh (nothing is written to disk unless the
//! caller asks), so a test can hand the root to its client and the client
//! verifies the server the same way it verifies a real provider.

use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use super::FakeError;

/// A self-signed root plus one server certificate for `localhost` and `127.0.0.1`.
#[derive(Clone)]
pub struct TestCa {
    ca_der: CertificateDer<'static>,
    ca_pem: String,
    server: Arc<rustls::ServerConfig>,
}

impl std::fmt::Debug for TestCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestCa").finish_non_exhaustive()
    }
}

fn tls_err(e: impl std::fmt::Display) -> FakeError {
    FakeError::Tls(e.to_string())
}

/// The ring provider, explicit so the fakes work whatever other crates in the
/// process have (or have not) installed as rustls' default.
pub(crate) fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl TestCa {
    /// Generates a new root and a server certificate signed by it.
    pub fn generate() -> Result<Self, FakeError> {
        let ca_key = KeyPair::generate().map_err(tls_err)?;
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).map_err(tls_err)?;
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "Steve fake mail test CA");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_cert = ca_params.self_signed(&ca_key).map_err(tls_err)?;
        let issuer = Issuer::new(ca_params, ca_key);

        let server_key = KeyPair::generate().map_err(tls_err)?;
        let mut server_params =
            CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                .map_err(tls_err)?;
        server_params
            .distinguished_name
            .push(DnType::CommonName, "localhost");
        let server_cert = server_params
            .signed_by(&server_key, &issuer)
            .map_err(tls_err)?;

        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der()));
        let server = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(tls_err)?
            .with_no_client_auth()
            .with_single_cert(vec![server_cert.der().clone(), ca_cert.der().clone()], key)
            .map_err(tls_err)?;

        Ok(Self {
            ca_der: ca_cert.der().clone(),
            ca_pem: ca_cert.pem(),
            server: Arc::new(server),
        })
    }

    /// The root certificate, PEM (what `STEVE_MAIL_TEST_CA` would point at).
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    /// The root certificate, DER.
    pub fn ca_der(&self) -> &CertificateDer<'static> {
        &self.ca_der
    }

    /// Server side config used by the fakes.
    pub fn server_config(&self) -> Arc<rustls::ServerConfig> {
        self.server.clone()
    }

    /// A client config that trusts only this root — convenience for tests.
    pub fn client_config(&self) -> Result<rustls::ClientConfig, FakeError> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.ca_der.clone()).map_err(tls_err)?;
        Ok(rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(tls_err)?
            .with_root_certificates(roots)
            .with_no_client_auth())
    }

    /// A tokio-rustls connector built from [`TestCa::client_config`].
    pub fn connector(&self) -> Result<tokio_rustls::TlsConnector, FakeError> {
        Ok(tokio_rustls::TlsConnector::from(Arc::new(
            self.client_config()?,
        )))
    }

    pub(crate) fn acceptor(&self) -> tokio_rustls::TlsAcceptor {
        tokio_rustls::TlsAcceptor::from(self.server.clone())
    }
}
