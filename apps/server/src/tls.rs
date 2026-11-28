//! TLS between a hub and its satellites, pinned to the hub's identity key.
//!
//! The hub presents its Ed25519 identity key as a raw public key (RFC
//! 7250), so there is no certificate and no authority: a satellite accepts
//! the one key it pinned when it joined, and nothing else. Only TLS 1.3,
//! only Ed25519, no resumption and no early data, so every connection is a
//! full handshake that the key signs. See "Hub and satellites" in
//! docs/internals/linking.md.

use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ritornel_engine::Signer;
use rustls::client::Resumption;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime};
use rustls::server::{AlwaysResolvesServerRawPublicKeys, NoServerSessionStorage};
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, PeerIncompatible, ServerConfig,
    SignatureAlgorithm, SignatureScheme,
};
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// What a TLS 1.3 record that opens a handshake starts with: the content
/// type of a handshake. A connection whose first byte is anything else is
/// plain HTTP.
pub(crate) const HANDSHAKE: u8 = 0x16;

/// The DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410): the
/// algorithm `1.3.101.112` and a bit string of 32 bytes. The key follows.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The one protocol spoken inside the TLS session.
const ALPN: &[u8] = b"http/1.1";

/// The raw public key a server presents for `key`, as TLS carries it.
fn spki(key: &[u8; 32]) -> Vec<u8> {
    [&ED25519_SPKI_PREFIX[..], key].concat()
}

/// The provider: aws-lc-rs, already in the build for the HTTP clients.
fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider())))
}

/// The server's identity key, as rustls signs with it. The seed stays in
/// the engine's [`Signer`].
#[derive(Debug)]
struct IdentityKey(Arc<Signer>);

impl rustls::sign::SigningKey for IdentityKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn rustls::sign::Signer>> {
        offered
            .contains(&SignatureScheme::ED25519)
            .then(|| Box::new(IdentitySigner(Arc::clone(&self.0))) as Box<dyn rustls::sign::Signer>)
    }

    fn public_key(&self) -> Option<SubjectPublicKeyInfoDer<'_>> {
        Some(SubjectPublicKeyInfoDer::from(spki(&self.0.public_key())))
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ED25519
    }
}

#[derive(Debug)]
struct IdentitySigner(Arc<Signer>);

impl rustls::sign::Signer for IdentitySigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        Ok(self.0.sign(message).to_vec())
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ED25519
    }
}

/// What the listener hands a connection that opens with a handshake.
pub(crate) fn acceptor(signer: Arc<Signer>) -> Result<TlsAcceptor, rustls::Error> {
    let key = Arc::new(CertifiedKey::new(
        vec![CertificateDer::from(spki(&signer.public_key()))],
        Arc::new(IdentityKey(signer)),
    ));
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(key)));
    // Every connection proves the key afresh: no ticket and no session
    // kept to resume from, and no early data.
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.max_early_data_size = 0;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Accepts the server whose identity key is `key`, and no other: the key
/// it presents must be that one, as a raw public key alone, and its
/// handshake must be signed by it with Ed25519.
#[derive(Debug)]
pub(crate) struct Pinned {
    key: [u8; 32],
}

impl Pinned {
    pub(crate) fn new(key: [u8; 32]) -> Self {
        Self { key }
    }
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if intermediates.is_empty()
            && ocsp_response.is_empty()
            && end_entity.as_ref() == spki(&self.key).as_slice()
        {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let signature = <[u8; 64]>::try_from(dss.signature()).ok();
        let valid = dss.scheme == SignatureScheme::ED25519
            && cert.as_ref() == spki(&self.key).as_slice()
            && signature.is_some_and(|signature| {
                ritornel_engine::verify_signature(&self.key, message, &signature)
            });
        if valid {
            Ok(HandshakeSignatureValid::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::BadSignature,
            ))
        }
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

/// The client side, for the hub whose key is `key`.
fn connector(key: [u8; 32]) -> Result<TlsConnector, rustls::Error> {
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned::new(key)))
        .with_no_client_auth();
    // A resumed handshake would not be checked against the key.
    config.resumption = Resumption::disabled();
    // The hub is named by its key, not by a name, and no name is sent.
    config.enable_sni = false;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Why a connection to the hub could not be made.
#[derive(Debug)]
pub(crate) enum DialError {
    /// Nothing answered at the address, or it refused.
    Unreachable(std::io::Error),
    /// Something answered, but not within the wait.
    TimedOut,
    /// A server answered over TLS with another key.
    WrongKey,
    /// Something answered, but not over TLS: a hub of an older release,
    /// or another program.
    NotTls,
    /// The handshake failed otherwise.
    Handshake(std::io::Error),
}

impl fmt::Display for DialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(error) => write!(f, "{error}"),
            Self::TimedOut => f.write_str("it did not answer in time"),
            Self::WrongKey => {
                f.write_str("another server answered there, not the hub the link names")
            }
            Self::NotTls => {
                f.write_str("it did not answer over TLS: the hub may be running an older release")
            }
            Self::Handshake(error) => write!(f, "the TLS handshake failed: {error}"),
        }
    }
}

/// What a failed handshake says about what answered.
fn handshake_failed(error: std::io::Error) -> DialError {
    let tls = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>());
    match tls {
        Some(rustls::Error::InvalidCertificate(_)) => DialError::WrongKey,
        Some(rustls::Error::InvalidMessage(_)) => DialError::NotTls,
        None if error.kind() == std::io::ErrorKind::UnexpectedEof => DialError::NotTls,
        _ => DialError::Handshake(error),
    }
}

/// Opens a TLS connection to the hub at `address` and checks that it holds
/// `key`, within `wait` for the connection and the handshake together.
/// Nothing of this server's is sent before the check: the credential goes
/// over the stream this returns.
pub(crate) async fn dial(
    address: SocketAddr,
    key: [u8; 32],
    wait: Duration,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, DialError> {
    tokio::time::timeout(wait, async {
        let tcp = TcpStream::connect(address)
            .await
            .map_err(DialError::Unreachable)?;
        handshake(tcp, key).await
    })
    .await
    .map_err(|_| DialError::TimedOut)?
}

/// The TLS handshake of [`dial`], on a connection already made, with no
/// wait of its own.
pub(crate) async fn handshake(
    tcp: TcpStream,
    key: [u8; 32],
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, DialError> {
    let address = tcp.peer_addr().map_err(DialError::Unreachable)?;
    connector(key)
        .map_err(|error| DialError::Handshake(std::io::Error::other(error)))?
        .connect(ServerName::from(address.ip()), tcp)
        .await
        .map_err(handshake_failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn signer(seed: u8) -> Arc<Signer> {
        Arc::new(Signer::from_seed([seed; 32]))
    }

    /// A server on loopback that completes each handshake with `acceptor`
    /// and answers `pong` to the first four bytes it is sent, or `name` if
    /// the client named the server it wanted.
    async fn serve_tls(acceptor: TlsAcceptor) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(mut tls) = acceptor.accept(tcp).await {
                        let named = tls.get_ref().1.server_name().is_some();
                        let mut asked = [0; 4];
                        if tls.read_exact(&mut asked).await.is_ok() {
                            let _ = tls.write_all(if named { b"name" } else { b"pong" }).await;
                        }
                    }
                });
            }
        });
        address
    }

    const WAIT: Duration = Duration::from_secs(60);

    #[tokio::test]
    async fn the_pinned_key_connects_with_a_full_handshake_each_time_and_no_name() {
        let hub = signer(7);
        let address = serve_tls(acceptor(Arc::clone(&hub)).unwrap()).await;
        // The second connection could resume the first if either side let
        // it: neither does.
        for _ in 0..2 {
            let mut tls = dial(address, hub.public_key(), WAIT).await.unwrap();
            let (_, session) = tls.get_ref();
            assert_eq!(
                session.protocol_version(),
                Some(rustls::ProtocolVersion::TLSv1_3)
            );
            assert_eq!(session.handshake_kind(), Some(rustls::HandshakeKind::Full));
            assert_eq!(session.alpn_protocol(), Some(ALPN));
            tls.write_all(b"ping").await.unwrap();
            let mut answer = [0; 4];
            tls.read_exact(&mut answer).await.unwrap();
            assert_eq!(&answer, b"pong", "the client sent a server name");
        }
    }

    /// Two connections on one connector to `address`, and how each
    /// handshake went.
    async fn twice(connector: &TlsConnector, address: SocketAddr) -> Vec<rustls::HandshakeKind> {
        let mut kinds = Vec::new();
        for _ in 0..2 {
            let tcp = TcpStream::connect(address).await.unwrap();
            let mut tls = connector
                .connect(ServerName::from(address.ip()), tcp)
                .await
                .unwrap();
            // Read the answer, which carries any ticket the server sends.
            tls.write_all(b"ping").await.unwrap();
            let mut answer = [0; 4];
            tls.read_exact(&mut answer).await.unwrap();
            kinds.push(tls.get_ref().1.handshake_kind().unwrap());
        }
        kinds
    }

    #[tokio::test]
    async fn neither_end_resumes_a_session_when_the_other_would() {
        use rustls::HandshakeKind::Full;
        let hub = signer(7);
        // The satellite's connector, against a server that issues tickets
        // and keeps sessions.
        let resuming_server = serve_tls(acceptor_with(
            Arc::new(IdentityKey(Arc::clone(&hub))),
            hub.public_key(),
        ))
        .await;
        let ours = connector(hub.public_key()).unwrap();
        assert_eq!(twice(&ours, resuming_server).await, [Full, Full]);
        // A client that would resume, against the hub's acceptor.
        let resuming_client = TlsConnector::from(Arc::new(
            ClientConfig::builder_with_provider(provider())
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Pinned::new(hub.public_key())))
                .with_no_client_auth(),
        ));
        let hubs = serve_tls(acceptor(Arc::clone(&hub)).unwrap()).await;
        assert_eq!(twice(&resuming_client, hubs).await, [Full, Full]);
    }

    #[tokio::test]
    async fn another_key_is_refused() {
        let address = serve_tls(acceptor(signer(7)).unwrap()).await;
        let pinned = signer(9).public_key();
        assert!(matches!(
            dial(address, pinned, WAIT).await,
            Err(DialError::WrongKey)
        ));
    }

    #[tokio::test]
    async fn plain_http_is_not_tls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        // As a hub of an older release answers: it reads the whole
        // ClientHello as a request it cannot parse, answers 400, and waits
        // for the client to close.
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut header = [0; 5];
            tcp.read_exact(&mut header).await.unwrap();
            let mut hello = vec![0; usize::from(u16::from_be_bytes([header[3], header[4]]))];
            tcp.read_exact(&mut hello).await.unwrap();
            tcp.write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
            let mut rest = Vec::new();
            let _ = tcp.read_to_end(&mut rest).await;
        });
        assert!(matches!(
            dial(address, signer(7).public_key(), WAIT).await,
            Err(DialError::NotTls)
        ));
    }

    /// A key that presents the pinned key but cannot sign with it: the
    /// public half is the hub's, the signature another's.
    #[derive(Debug)]
    struct Impostor {
        shown: [u8; 32],
        signs: Arc<Signer>,
    }

    impl rustls::sign::SigningKey for Impostor {
        fn choose_scheme(
            &self,
            offered: &[SignatureScheme],
        ) -> Option<Box<dyn rustls::sign::Signer>> {
            IdentityKey(Arc::clone(&self.signs)).choose_scheme(offered)
        }

        fn public_key(&self) -> Option<SubjectPublicKeyInfoDer<'_>> {
            Some(SubjectPublicKeyInfoDer::from(spki(&self.shown)))
        }

        fn algorithm(&self) -> SignatureAlgorithm {
            SignatureAlgorithm::ED25519
        }
    }

    fn acceptor_with(key: Arc<dyn rustls::sign::SigningKey>, shown: [u8; 32]) -> TlsAcceptor {
        let key = Arc::new(CertifiedKey::new(
            vec![CertificateDer::from(spki(&shown))],
            key,
        ));
        let config = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(key)));
        TlsAcceptor::from(Arc::new(config))
    }

    #[tokio::test]
    async fn the_pinned_key_without_its_signature_is_refused() {
        let hub = signer(7).public_key();
        let impostor = Impostor {
            shown: hub,
            signs: signer(9),
        };
        let address = serve_tls(acceptor_with(Arc::new(impostor), hub)).await;
        assert!(matches!(
            dial(address, hub, WAIT).await,
            Err(DialError::WrongKey)
        ));
    }

    #[test]
    fn a_certificate_chain_or_a_stapled_answer_is_refused() {
        let key = signer(7).public_key();
        let pinned = Pinned::new(key);
        let end = CertificateDer::from(spki(&key));
        let name = ServerName::from(std::net::IpAddr::from([127, 0, 0, 1]));
        let now = UnixTime::now();
        assert!(
            pinned
                .verify_server_cert(&end, &[], &name, &[], now)
                .is_ok()
        );
        assert!(
            pinned
                .verify_server_cert(&end, std::slice::from_ref(&end), &name, &[], now)
                .is_err()
        );
        assert!(
            pinned
                .verify_server_cert(&end, &[], &name, &[1], now)
                .is_err()
        );
        let longer = CertificateDer::from([spki(&key), vec![0]].concat());
        assert!(
            pinned
                .verify_server_cert(&longer, &[], &name, &[], now)
                .is_err()
        );
    }

    #[test]
    fn only_tls_1_3_and_ed25519_are_offered() {
        let pinned = Pinned::new(signer(7).public_key());
        assert_eq!(
            pinned.supported_verify_schemes(),
            vec![SignatureScheme::ED25519]
        );
        assert!(pinned.requires_raw_public_keys());
    }
}
