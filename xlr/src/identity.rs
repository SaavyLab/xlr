//! This machine's identity: a key pair and self-signed certificate created
//! on first use. The certificate's SHA-256 fingerprint is the machine's ID;
//! peers pin it, so no certificate authority is involved.

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use sha2::{Digest, Sha256};
use std::{fmt, fs, io::Write as _, path::Path};

/// A certificate fingerprint: lowercase hex SHA-256 of the DER certificate.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Fingerprint(String);

impl Fingerprint {
    pub fn of(certificate: &[u8]) -> Self {
        Self(
            Sha256::digest(certificate)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    /// Parses a full fingerprint as stored in files.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text
            .trim()
            .to_ascii_lowercase()
            .replace([':', '-', ' '], "");
        if text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(text))
        } else {
            Err(format!("`{text}` is not a SHA-256 fingerprint"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first 16 hex digits in groups of four, for display.
    pub fn short(&self) -> String {
        self.0.as_bytes()[..16]
            .chunks(4)
            .map(|chunk| std::str::from_utf8(chunk).expect("hex is ASCII"))
            .collect::<Vec<_>>()
            .join("-")
    }

    fn bytes(&self) -> Vec<u8> {
        (0..self.0.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&self.0[at..at + 2], 16).expect("validated hex"))
            .collect()
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The six-digit code both sides show while pairing. It depends on both
/// fingerprints, so an interceptor in the middle produces different codes on
/// each side.
pub fn pairing_code(server: &Fingerprint, client: &Fingerprint) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"xlr pairing v1");
    hasher.update(server.bytes());
    hasher.update(client.bytes());
    let digest = hasher.finalize();
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03} {:03}", value / 1000, value % 1000)
}

pub struct Identity {
    pub certificate: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
    pub fingerprint: Fingerprint,
}

impl Identity {
    /// Loads `<home>/identity/{cert,key}.pem`, creating them on first use.
    pub fn load_or_create(home: &Path) -> Result<Self, String> {
        let dir = home.join("identity");
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        if !cert_path.exists() {
            create(&dir, &cert_path, &key_path)?;
        }
        let certificate = CertificateDer::from_pem_file(&cert_path)
            .map_err(|error| format!("{}: {error}", cert_path.display()))?;
        let key = PrivateKeyDer::from_pem_file(&key_path)
            .map_err(|error| format!("{}: {error}", key_path.display()))?;
        Ok(Self {
            fingerprint: Fingerprint::of(&certificate),
            certificate,
            key,
        })
    }
}

fn create(dir: &Path, cert_path: &Path, key_path: &Path) -> Result<(), String> {
    let key = rcgen::KeyPair::generate().map_err(|error| error.to_string())?;
    let mut params =
        rcgen::CertificateParams::new(vec!["xlr".to_owned()]).map_err(|error| error.to_string())?;
    params.not_after = rcgen::date_time_ymd(4000, 1, 1);
    let certificate = params
        .self_signed(&key)
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    write_private(key_path, key.serialize_pem().as_bytes())?;
    fs::write(cert_path, certificate.pem())
        .map_err(|error| format!("{}: {error}", cert_path.display()))
}

/// Writes a file readable only by its owner.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(contents))
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_parse_display_and_shorten() {
        let fingerprint = Fingerprint::of(b"certificate");
        assert_eq!(
            Fingerprint::parse(fingerprint.as_str()),
            Ok(fingerprint.clone())
        );
        assert_eq!(fingerprint.short().len(), 19);
        assert!(Fingerprint::parse("abc").is_err());
    }

    #[test]
    fn pairing_codes_depend_on_both_sides_and_order() {
        let server = Fingerprint::of(b"server");
        let client = Fingerprint::of(b"client");
        let code = pairing_code(&server, &client);
        assert_eq!(code.len(), 7);
        assert_eq!(code, pairing_code(&server, &client));
        assert_ne!(code, pairing_code(&client, &server));
        assert_ne!(code, pairing_code(&server, &Fingerprint::of(b"other")));
    }

    #[test]
    fn identity_is_created_once_and_reloaded() {
        let home = std::env::temp_dir().join(format!("xlr-identity-test-{}", std::process::id()));
        let first = Identity::load_or_create(&home).unwrap();
        let second = Identity::load_or_create(&home).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(home.join("identity/key.pem"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(home).unwrap();
    }
}
