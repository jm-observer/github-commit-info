use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose};

use super::ca::CertAuthority;

/// Cached domain certificate: DER-encoded cert and key pair.
struct CachedCert {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

/// Generates and caches TLS certificates for domains, signed by the CA.
pub struct CertCache {
    ca: CertAuthority,
    cache: Mutex<HashMap<String, CachedCert>>,
}

impl CertCache {
    /// Create a new certificate cache backed by the given CA.
    pub fn new(ca: CertAuthority) -> Self {
        Self {
            ca,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Get or create a certificate for the given domain.
    /// Returns (cert_der, key_der).
    pub fn get_or_create(&self, domain: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(cached) = cache.get(domain) {
            return Ok((cached.cert_der.clone(), cached.key_der.clone()));
        }

        let key_pair = KeyPair::generate()?;
        let mut params = CertificateParams::new(vec![domain.to_string()])?;
        params.distinguished_name.push(DnType::CommonName, domain);
        params.is_ca = IsCa::ExplicitNoCa;
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params.signed_by(&key_pair, &self.ca.ca_cert, &self.ca.ca_key_pair)?;

        let cert_der = cert.der().to_vec();
        let key_der = key_pair.serialize_der().to_vec();

        cache.insert(
            domain.to_string(),
            CachedCert {
                cert_der: cert_der.clone(),
                key_der: key_der.clone(),
            },
        );

        Ok((cert_der, key_der))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_site_cert() {
        let ca = CertAuthority::generate().unwrap();
        let cache = CertCache::new(ca);

        let (cert_der, key_der) = cache.get_or_create("api.openai.com").unwrap();
        assert!(!cert_der.is_empty());
        assert!(!key_der.is_empty());
    }

    #[test]
    fn test_cache_returns_same_cert() {
        let ca = CertAuthority::generate().unwrap();
        let cache = CertCache::new(ca);

        let (cert1, key1) = cache.get_or_create("example.com").unwrap();
        let (cert2, key2) = cache.get_or_create("example.com").unwrap();
        assert_eq!(cert1, cert2);
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_different_domains_different_certs() {
        let ca = CertAuthority::generate().unwrap();
        let cache = CertCache::new(ca);

        let (cert1, _) = cache.get_or_create("a.com").unwrap();
        let (cert2, _) = cache.get_or_create("b.com").unwrap();
        assert_ne!(cert1, cert2);
    }
}
