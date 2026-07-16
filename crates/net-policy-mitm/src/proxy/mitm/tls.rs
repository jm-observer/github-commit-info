use std::sync::Arc;

use anyhow::{Context, Result};
use log::info;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::server::TlsStream as ServerTlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

pub(super) fn build_server_config(
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    force_h1: bool,
) -> Result<ServerConfig> {
    let certs = vec![CertificateDer::from(cert_der)];
    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key_der));
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    // If forcing H1, we only offer http/1.1 to the client.
    // Otherwise, offer both h2 and http/1.1 — HTTP/1.1 traffic is parsed and handed
    // to the FlowSink, HTTP/2 (gRPC) traffic is parsed per-stream in http2.rs.
    if force_h1 {
        info!("Forcing HTTP/1.1 for domain (ALPN)");
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
    } else {
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }
    Ok(config)
}

pub(super) fn build_client_config(alpn: &[u8]) -> Result<ClientConfig> {
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    // Mirror the ALPN protocol negotiated with the client
    config.alpn_protocols = vec![alpn.to_vec()];
    Ok(config)
}

pub(super) async fn accept_client(
    tcp: TcpStream,
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    force_h1: bool,
) -> Result<(ServerTlsStream<TcpStream>, Vec<u8>)> {
    let server_config = build_server_config(cert_der, key_der, force_h1)?;
    let acceptor = TlsAcceptor::from(Arc::new(server_config));
    let client_tls = acceptor
        .accept(tcp)
        .await
        .context("TLS handshake with client (MITM)")?;

    let negotiated_alpn = client_tls
        .get_ref()
        .1
        .alpn_protocol()
        .unwrap_or(b"http/1.1")
        .to_vec();

    Ok((client_tls, negotiated_alpn))
}

pub(super) async fn connect_upstream(
    tcp: TcpStream,
    domain: &str,
    alpn: &[u8],
) -> Result<ClientTlsStream<TcpStream>> {
    let client_config = build_client_config(alpn)?;
    let connector = TlsConnector::from(Arc::new(client_config));
    let server_name = ServerName::try_from(domain.to_string())
        .map_err(|e| anyhow::anyhow!("Invalid server name '{domain}': {e}"))?;
    let upstream_tls = connector
        .connect(server_name, tcp)
        .await
        .context("TLS handshake with upstream server")?;

    Ok(upstream_tls)
}
