use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};

/// Holds the CA certificate (PEM) and its key pair, used to sign site certificates.
pub struct CertAuthority {
    pub ca_cert_pem: String,
    pub ca_key_pair: KeyPair,
    /// The parsed CA certificate for signing site certs.
    pub ca_cert: rcgen::Certificate,
}

impl CertAuthority {
    /// Generate a new self-signed CA certificate and key pair.
    pub fn generate() -> Result<Self> {
        let key_pair = KeyPair::generate()?;

        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "net-policy-mitm CA");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "net-policy-mitm");
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        // Default validity is very long (1975-4096), which is fine for a local dev CA.

        let ca_cert = params.self_signed(&key_pair)?;
        let ca_cert_pem = ca_cert.pem();

        Ok(Self {
            ca_cert_pem,
            ca_key_pair: key_pair,
            ca_cert,
        })
    }

    /// The CA certificate in DER form (for computing a SHA-256 thumbprint, or feeding a
    /// platform trust-store install API that wants DER).
    pub fn ca_cert_der(&self) -> Vec<u8> {
        self.ca_cert.der().as_ref().to_vec()
    }

    /// Load an existing CA from PEM files in the given directory.
    pub fn load(ca_dir: &Path) -> Result<Self> {
        let cert_path = ca_dir.join("ca.crt");
        let key_path = ca_dir.join("ca.key");

        let ca_cert_pem = fs::read_to_string(&cert_path)
            .with_context(|| format!("Reading {}", cert_path.display()))?;
        let ca_key_pem = fs::read_to_string(&key_path)
            .with_context(|| format!("Reading {}", key_path.display()))?;

        let key_pair = KeyPair::from_pem(&ca_key_pem)?;

        let params = CertificateParams::from_ca_cert_pem(&ca_cert_pem)?;
        let ca_cert = params.self_signed(&key_pair)?;

        Ok(Self {
            ca_cert_pem,
            ca_key_pair: key_pair,
            ca_cert,
        })
    }

    /// Construct a `CertAuthority` from in-memory PEM strings, without touching disk.
    ///
    /// This is what net-policy's agent uses so the CA **private key never lands on
    /// disk in plaintext**: at rest the key is DPAPI-encrypted (machine scope, §17.4);
    /// at session start the agent decrypts it in memory and builds the authority here.
    pub fn from_pem(ca_cert_pem: &str, ca_key_pem: &str) -> Result<Self> {
        let key_pair = KeyPair::from_pem(ca_key_pem)?;
        let params = CertificateParams::from_ca_cert_pem(ca_cert_pem)?;
        let ca_cert = params.self_signed(&key_pair)?;
        Ok(Self {
            ca_cert_pem: ca_cert_pem.to_string(),
            ca_key_pair: key_pair,
            ca_cert,
        })
    }

    /// The CA private key serialized as PEM. Callers that persist this **must**
    /// protect it (DPAPI + ACL on Windows); it is secret material.
    pub fn ca_key_pem(&self) -> String {
        self.ca_key_pair.serialize_pem()
    }

    /// Save the CA certificate and key to PEM files in the given directory.
    pub fn save(&self, ca_dir: &Path) -> Result<()> {
        fs::create_dir_all(ca_dir)?;

        let cert_path = ca_dir.join("ca.crt");
        let key_path = ca_dir.join("ca.key");

        fs::write(&cert_path, &self.ca_cert_pem)?;
        fs::write(&key_path, self.ca_key_pair.serialize_pem())?;

        // Set key file permissions to 600 (owner read/write only)
        // Windows 下私钥保护由 net-policy agent 用 ACL + DPAPI 处理（见抓包设计 §17.4）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    /// Load existing CA from directory, or generate and save a new one.
    pub fn load_or_generate(ca_dir: &Path) -> Result<Self> {
        if ca_dir.join("ca.crt").exists() && ca_dir.join("ca.key").exists() {
            Self::load(ca_dir)
        } else {
            let ca = Self::generate()?;
            ca.save(ca_dir)?;
            Ok(ca)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_generate_ca() {
        let ca = CertAuthority::generate().unwrap();
        assert!(ca.ca_cert_pem.contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn test_save_and_load() {
        let dir = temp_dir();
        let ca = CertAuthority::generate().unwrap();
        ca.save(dir.path()).unwrap();

        assert!(dir.path().join("ca.crt").exists());
        assert!(dir.path().join("ca.key").exists());

        let loaded = CertAuthority::load(dir.path()).unwrap();
        assert_eq!(ca.ca_cert_pem, loaded.ca_cert_pem);
    }

    #[test]
    fn test_load_or_generate_creates_new() {
        let dir = temp_dir();
        let ca = CertAuthority::load_or_generate(dir.path()).unwrap();
        assert!(dir.path().join("ca.crt").exists());
        assert!(ca.ca_cert_pem.contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn test_load_or_generate_loads_existing() {
        let dir = temp_dir();
        let ca1 = CertAuthority::load_or_generate(dir.path()).unwrap();
        let ca2 = CertAuthority::load_or_generate(dir.path()).unwrap();
        assert_eq!(ca1.ca_cert_pem, ca2.ca_cert_pem);
    }

    #[cfg(unix)]
    #[test]
    fn test_key_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let ca = CertAuthority::generate().unwrap();
        ca.save(dir.path()).unwrap();

        let key_path = dir.path().join("ca.key");
        let perms = fs::metadata(&key_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }
}
