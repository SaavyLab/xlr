//! TLS 1.3 with self-signed certificates identified by fingerprint.
//!
//! Neither side validates a certificate chain. The client checks that the
//! server presents the pinned fingerprint; the server accepts any client
//! certificate at the TLS layer and authorizes its fingerprint afterwards
//! against the approved peers. Both sides still verify the handshake
//! signatures, so each peer proves it holds the private key.

use crate::identity::{Fingerprint, Identity};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, WebPkiSupportedAlgorithms, ring},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    version::TLS13,
};
use std::sync::{Arc, Mutex};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

fn algorithms() -> WebPkiSupportedAlgorithms {
    ring::default_provider().signature_verification_algorithms
}

/// Client side: accepts only the pinned fingerprint, or, when nothing is
/// pinned yet (first contact), records what the server presented.
#[derive(Debug)]
pub struct PinnedServer {
    expected: Option<Fingerprint>,
    seen: Mutex<Option<Fingerprint>>,
}

impl PinnedServer {
    pub fn new(expected: Option<Fingerprint>) -> Arc<Self> {
        Arc::new(Self {
            expected,
            seen: Mutex::new(None),
        })
    }

    /// The fingerprint the server presented, once the handshake has run.
    pub fn seen(&self) -> Option<Fingerprint> {
        self.seen.lock().expect("not poisoned").clone()
    }
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let fingerprint = Fingerprint::of(end_entity);
        *self.seen.lock().expect("not poisoned") = Some(fingerprint.clone());
        match &self.expected {
            Some(expected) if *expected != fingerprint => Err(Error::General(format!(
                "host presented {} but {} is pinned; it may have been reinstalled, or \
                 something is intercepting the connection",
                fingerprint.short(),
                expected.short()
            ))),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &algorithms())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &algorithms())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms().supported_schemes()
    }
}

/// Server side: requires a client certificate and a valid handshake
/// signature; authorization by fingerprint happens after the handshake.
#[derive(Debug)]
struct AnyClient;

impl ClientCertVerifier for AnyClient {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &algorithms())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &algorithms())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms().supported_schemes()
    }
}

pub fn server_config(identity: &Identity) -> Result<Arc<ServerConfig>, Error> {
    Ok(Arc::new(
        ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&TLS13])?
            .with_client_cert_verifier(Arc::new(AnyClient))
            .with_single_cert(vec![identity.certificate.clone()], identity.key.clone_key())?,
    ))
}

pub fn client_config(
    identity: &Identity,
    verifier: Arc<PinnedServer>,
) -> Result<Arc<ClientConfig>, Error> {
    Ok(Arc::new(
        ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![identity.certificate.clone()], identity.key.clone_key())?,
    ))
}
