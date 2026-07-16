pub mod http;
pub mod socks5;

use anyhow::{bail, Result};
use std::net::SocketAddr;
use tokio::net::TcpStream;

/// Upstream proxy type — either SOCKS5 or HTTP CONNECT.
pub enum Upstream {
    Socks5(SocketAddr),
    HttpConnect(SocketAddr),
}

impl Upstream {
    /// Parse an upstream proxy URL string.
    pub fn parse(url: &str) -> Result<Self> {
        if let Some(rest) = url.strip_prefix("socks5://") {
            let addr: SocketAddr = rest.parse()?;
            Ok(Upstream::Socks5(addr))
        } else if let Some(rest) = url.strip_prefix("http://") {
            let addr: SocketAddr = rest.parse()?;
            Ok(Upstream::HttpConnect(addr))
        } else {
            bail!(
                "Unsupported upstream URL: {url}\nExpected: socks5://host:port or http://host:port"
            )
        }
    }

    /// Connect to target_host:target_port through this upstream proxy.
    /// target_host must be a domain name — never resolve DNS internally.
    pub async fn connect(&self, target_host: &str, target_port: u16) -> Result<TcpStream> {
        match self {
            Upstream::Socks5(addr) => socks5::connect(*addr, target_host, target_port).await,
            Upstream::HttpConnect(addr) => http::connect(*addr, target_host, target_port).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_socks5_url() {
        let u = Upstream::parse("socks5://127.0.0.1:7890").unwrap();
        assert!(matches!(u, Upstream::Socks5(addr) if addr.port() == 7890));
    }

    #[test]
    fn parse_http_url() {
        let u = Upstream::parse("http://127.0.0.1:7890").unwrap();
        assert!(matches!(u, Upstream::HttpConnect(addr) if addr.port() == 7890));
    }

    #[test]
    fn parse_invalid_url() {
        assert!(Upstream::parse("ftp://127.0.0.1:7890").is_err());
        assert!(Upstream::parse("garbage").is_err());
    }
}
