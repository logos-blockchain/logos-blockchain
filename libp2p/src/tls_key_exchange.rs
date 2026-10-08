//! Checks on the TLS configuration that `libp2p-quic` builds for every
//! connection: the hybrid post-quantum `X25519MLKEM768` key exchange must be
//! negotiated between two up-to-date peers, and a classical-only peer must
//! still be able to connect over `X25519`.

use std::sync::Arc;

use libp2p::{PeerId, identity, tls};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, NamedGroup, ServerConfig,
    ServerConnection, SignatureScheme,
    client::{
        ResolvesClientCert,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, ServerName, UnixTime},
    sign::CertifiedKey,
};

const P2P_ALPN: &[u8] = b"libp2p";
/// Generous upper bound on the number of flights a TLS 1.3 handshake may take.
const MAX_HANDSHAKE_ROUNDS: usize = 16;

/// Drives an in-memory TLS handshake between `client` and `server` until both
/// sides have finished, shuttling records back and forth.
fn complete_handshake(client: &mut ClientConnection, server: &mut ServerConnection) {
    let mut wire = Vec::new();
    for _ in 0..MAX_HANDSHAKE_ROUNDS {
        if !client.is_handshaking() && !server.is_handshaking() {
            return;
        }

        wire.clear();
        while client.wants_write() {
            client
                .write_tls(&mut wire)
                .expect("client writes TLS records");
        }
        let mut incoming = wire.as_slice();
        while !incoming.is_empty() {
            server
                .read_tls(&mut incoming)
                .expect("server reads TLS records");
        }
        server
            .process_new_packets()
            .expect("server accepts client flight");

        wire.clear();
        while server.wants_write() {
            server
                .write_tls(&mut wire)
                .expect("server writes TLS records");
        }
        let mut incoming = wire.as_slice();
        while !incoming.is_empty() {
            client
                .read_tls(&mut incoming)
                .expect("client reads TLS records");
        }
        client
            .process_new_packets()
            .expect("client accepts server flight");
    }
    panic!("TLS handshake did not complete within {MAX_HANDSHAKE_ROUNDS} rounds");
}

fn connect(
    client_config: ClientConfig,
    server_config: ServerConfig,
) -> (ClientConnection, ServerConnection) {
    // `libp2p-quic` dials with this placeholder name; libp2p's verifier ignores it.
    let server_name = ServerName::try_from("l").expect("valid placeholder server name");
    let mut client =
        ClientConnection::new(Arc::new(client_config), server_name).expect("client connection");
    let mut server = ServerConnection::new(Arc::new(server_config)).expect("server connection");
    complete_handshake(&mut client, &mut server);
    (client, server)
}

fn negotiated_group(client: &ClientConnection, server: &ServerConnection) -> NamedGroup {
    let client_group = client
        .negotiated_key_exchange_group()
        .expect("client finished key exchange")
        .name();
    let server_group = server
        .negotiated_key_exchange_group()
        .expect("server finished key exchange")
        .name();
    assert_eq!(client_group, server_group, "both sides agree on the group");
    client_group
}

/// Test-only verifier that accepts whatever certificate the server presents.
/// It stands in for the verifier of an older peer, so only the key exchange
/// is under test here.
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
        aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Always presents the given libp2p certificate. `libp2p-tls` does the same
/// internally; the generic `with_client_auth_cert` path cannot be used because
/// the libp2p certificate carries a critical extension webpki does not know.
#[derive(Debug)]
struct AlwaysPresentCert(Arc<CertifiedKey>);

impl AlwaysPresentCert {
    fn for_keypair(keypair: &identity::Keypair) -> Self {
        let (certificate, private_key) =
            tls::certificate::generate(keypair).expect("libp2p certificate");
        let signing_key =
            aws_lc_rs::sign::any_ecdsa_type(&private_key).expect("libp2p certificate key");
        Self(Arc::new(CertifiedKey::new(vec![certificate], signing_key)))
    }
}

impl ResolvesClientCert for AlwaysPresentCert {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[SignatureScheme],
    ) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// A client that speaks the libp2p TLS profile (libp2p certificate, ALPN) but
/// only offers the classical `X25519` group, as a peer built before the
/// post-quantum switch would.
fn classical_only_client_config(keypair: &identity::Keypair) -> ClientConfig {
    let mut provider = aws_lc_rs::default_provider();
    provider.kx_groups = vec![aws_lc_rs::kx_group::X25519];

    let mut config = ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 is supported")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
        .with_client_cert_resolver(Arc::new(AlwaysPresentCert::for_keypair(keypair)));
    config.alpn_protocols = vec![P2P_ALPN.to_vec()];
    config
}

#[test]
fn libp2p_peers_negotiate_hybrid_post_quantum_key_exchange() {
    let client_keypair = identity::Keypair::generate_ed25519();
    let server_keypair = identity::Keypair::generate_ed25519();
    let server_peer_id = PeerId::from(server_keypair.public());

    let client_config =
        tls::make_client_config(&client_keypair, Some(server_peer_id)).expect("client config");
    let server_config = tls::make_server_config(&server_keypair).expect("server config");

    let (client, server) = connect(client_config, server_config);

    assert_eq!(
        negotiated_group(&client, &server),
        NamedGroup::X25519MLKEM768,
        "two up-to-date libp2p peers must use the hybrid ML-KEM key exchange"
    );
}

#[test]
fn classical_only_peer_still_connects_over_x25519() {
    let client_keypair = identity::Keypair::generate_ed25519();
    let server_keypair = identity::Keypair::generate_ed25519();

    let client_config = classical_only_client_config(&client_keypair);
    let server_config = tls::make_server_config(&server_keypair).expect("server config");

    let (client, server) = connect(client_config, server_config);

    assert_eq!(
        negotiated_group(&client, &server),
        NamedGroup::X25519,
        "a peer without ML-KEM support must fall back to X25519"
    );
}
