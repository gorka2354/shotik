//! Signed updates. The application trusts one small manifest, signed with a key that is not on GitHub, and checks the
//! installer against the hash inside it. So whoever takes over the GitHub account can replace every file on the release
//! page and still cannot get code run on a user's machine. See `docs/rust/UPDATES.md`.
//!
//! The order is the point: the signature is checked before the manifest is even parsed.

use std::time::{Duration, SystemTime};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// The other half stays on the machine that publishes. `installer/sign-update.js` refuses any other key.
pub const UPDATE_PUBLIC_KEY: &str = "c65465923883391a4634c9b8b03e6dd7023e7f04c08ebdd9a7aaeb67911b0fd8";
const RELEASES: &str = "https://github.com/gorka2354/shotik/releases";
/// Keeps a signature made for anything else from ever passing for a manifest's.
const DOMAIN: &[u8] = b"shotik-update-manifest-v1\n";
/// How long a successful check stands. A failed one is retried at the next look, an hour later.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    /// Exactly three numbers. It ends up in a URL and a file name, so nothing else gets through.
    pub fn parse(text: &str) -> Result<Self> {
        let mut parts = text.split('.');
        let mut number = || -> Option<u32> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let parsed = (number(), number(), number());
        let invalid = || Error::msg(format!("not a version: {:?}", text.chars().take(32).collect::<String>()));
        match (parsed, parts.next()) {
            ((Some(major), Some(minor), Some(patch)), None) => Ok(Self { major, minor, patch }),
            _ => Err(invalid()),
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: Version,
    pub sha256: [u8; 32],
    pub size: u64,
}

#[derive(Deserialize)]
struct RawManifest {
    version: String,
    sha256: String,
    size: u64,
}

/// Where a build looks for updates, and the key it trusts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSource {
    latest_base: String,
    installer_base: Option<String>,
    releases: String,
    pub public_key: String,
}

impl UpdateSource {
    pub fn github() -> Self {
        Self {
            latest_base: format!("{RELEASES}/latest/download"),
            installer_base: None,
            releases: RELEASES.to_string(),
            public_key: UPDATE_PUBLIC_KEY.to_string(),
        }
    }

    /// For tests: everything under one address, signed with their own key.
    pub fn at(base: &str, public_key: &str) -> Self {
        Self {
            latest_base: base.to_string(),
            installer_base: Some(base.to_string()),
            releases: base.to_string(),
            public_key: public_key.to_string(),
        }
    }

    pub fn manifest_url(&self) -> String {
        format!("{}/latest.json", self.latest_base)
    }

    pub fn signature_url(&self) -> String {
        format!("{}/latest.json.sig", self.latest_base)
    }

    /// Built from the verified version: nothing from the network decides where the installer comes from.
    pub fn installer_url(&self, version: Version) -> String {
        match &self.installer_base {
            Some(base) => format!("{base}/{}", installer_name(version)),
            None => format!("{}/download/v{version}/{}", self.releases, installer_name(version)),
        }
    }

    pub fn release_page(&self, version: Version) -> String {
        format!("{}/tag/v{version}", self.releases)
    }
}

pub fn installer_name(version: Version) -> String {
    format!("Shotik-Setup-{version}.exe")
}

fn from_hex<const N: usize>(text: &str, what: &str) -> Result<[u8; N]> {
    let bytes = text.as_bytes();
    let bad = || Error::msg(format!("the {what} is not {} hex characters", N * 2));
    if bytes.len() != N * 2 {
        return Err(bad());
    }
    let digit = |b: u8| char::from(b).to_digit(16).map(|d| d as u8);
    let mut out = [0u8; N];
    for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        *slot = digit(pair[0]).zip(digit(pair[1])).map(|(high, low)| high << 4 | low).ok_or_else(bad)?;
    }
    Ok(out)
}

/// The manifest, only if its signature holds.
pub fn verify_manifest(manifest: &[u8], signature: &[u8], public_key: &str) -> Result<Manifest> {
    let key = VerifyingKey::from_bytes(&from_hex::<32>(public_key, "public key")?)
        .map_err(|_| Error::msg("the update key is not valid"))?;
    let signature = std::str::from_utf8(signature).map_err(|_| Error::msg("the update signature is not text"))?;
    let signature = Signature::from_bytes(&from_hex::<64>(signature.trim(), "update signature")?);
    let mut signed = Vec::with_capacity(DOMAIN.len() + manifest.len());
    signed.extend_from_slice(DOMAIN);
    signed.extend_from_slice(manifest);
    key.verify_strict(&signed, &signature).map_err(|_| Error::msg("the update signature does not match"))?;

    let raw: RawManifest =
        serde_json::from_slice(manifest).map_err(|e| Error::msg(format!("the update manifest cannot be read: {e}")))?;
    Ok(Manifest { version: Version::parse(&raw.version)?, sha256: from_hex(&raw.sha256, "installer hash")?, size: raw.size })
}

/// Whether this is the very file the signed manifest describes.
pub fn check_installer(installer: &[u8], manifest: &Manifest) -> Result<()> {
    if installer.len() as u64 != manifest.size {
        return Err(Error::msg(format!(
            "the downloaded update is {} bytes, the signed size is {}",
            installer.len(),
            manifest.size
        )));
    }
    let hash: [u8; 32] = Sha256::digest(installer).into();
    if hash != manifest.sha256 {
        return Err(Error::msg("the downloaded update does not match its signed hash"));
    }
    Ok(())
}

/// Whether a background check should run now. A clock moved back counts as due: it must not silence updates.
pub fn check_is_due(last_success: Option<SystemTime>, now: SystemTime) -> bool {
    last_success.is_none_or(|last| now.duration_since(last).map_or(true, |elapsed| elapsed >= CHECK_INTERVAL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn public() -> String {
        hex(key().verifying_key().as_bytes())
    }

    fn sign_with(key: &SigningKey, manifest: &[u8]) -> Vec<u8> {
        let mut signed = DOMAIN.to_vec();
        signed.extend_from_slice(manifest);
        hex(&key.sign(&signed).to_bytes()).into_bytes()
    }

    fn manifest_for(installer: &[u8], version: &str) -> Vec<u8> {
        let hash: [u8; 32] = Sha256::digest(installer).into();
        format!(r#"{{"version":"{version}","sha256":"{}","size":{}}}"#, hex(&hash), installer.len()).into_bytes()
    }

    #[test]
    fn a_signed_manifest_is_read() {
        let bytes = manifest_for(b"installer", "2.1.0");
        let mut signature = sign_with(&key(), &bytes);
        signature.push(b'\n');
        let manifest = verify_manifest(&bytes, &signature, &public()).unwrap();
        assert_eq!(manifest.version, Version::parse("2.1.0").unwrap());
        assert_eq!(manifest.size, 9);
        check_installer(b"installer", &manifest).unwrap();
    }

    #[test]
    fn any_change_to_the_manifest_breaks_the_signature() {
        let bytes = manifest_for(b"installer", "2.1.0");
        let signature = sign_with(&key(), &bytes);
        let forged = manifest_for(b"malware!!", "2.1.0");
        assert!(verify_manifest(&forged, &signature, &public()).is_err());
        let newer = manifest_for(b"installer", "9.9.9");
        assert!(verify_manifest(&newer, &signature, &public()).is_err());
        let mut flipped = bytes.clone();
        flipped[3] ^= 1;
        assert!(verify_manifest(&flipped, &signature, &public()).is_err());
    }

    #[test]
    fn any_change_to_the_signature_breaks_it() {
        let bytes = manifest_for(b"installer", "2.1.0");
        let good = sign_with(&key(), &bytes);
        for position in [0, 40, 127] {
            let mut bad = good.clone();
            bad[position] = if bad[position] == b'0' { b'1' } else { b'0' };
            assert!(verify_manifest(&bytes, &bad, &public()).is_err(), "position {position}");
        }
    }

    #[test]
    fn another_key_is_refused() {
        let bytes = manifest_for(b"installer", "2.1.0");
        let thief = SigningKey::from_bytes(&[9u8; 32]);
        let error = verify_manifest(&bytes, &sign_with(&thief, &bytes), &public()).unwrap_err();
        assert!(error.to_string().contains("signature does not match"), "{error}");
    }

    #[test]
    fn a_signature_made_for_something_else_is_refused() {
        // The same key over the bare bytes, without the domain.
        let bytes = manifest_for(b"installer", "2.1.0");
        let bare = hex(&key().sign(&bytes).to_bytes()).into_bytes();
        assert!(verify_manifest(&bytes, &bare, &public()).is_err());
    }

    #[test]
    fn a_broken_signature_or_key_is_an_error_not_a_panic() {
        let bytes = manifest_for(b"installer", "2.1.0");
        for signature in [&b""[..], b"zz", b"abcd", &[0xff; 128], &[b'0'; 128], &[b'g'; 128]] {
            assert!(verify_manifest(&bytes, signature, &public()).is_err());
        }
        let signature = sign_with(&key(), &bytes);
        for public_key in ["", "abcd", &"zz".repeat(32)] {
            assert!(verify_manifest(&bytes, &signature, public_key).is_err());
        }
    }

    #[test]
    fn a_signed_but_unreadable_manifest_is_an_error() {
        for bytes in [
            &b"not json"[..],
            br#"{"version":"2.1.0"}"#,
            br#"{"version":"2.1","sha256":"00","size":1}"#,
            br#"{"version":"2.1.0/../../evil","sha256":"00","size":1}"#,
            br#"{"version":"2.1.0","sha256":"nothex","size":1}"#,
        ] {
            assert!(verify_manifest(bytes, &sign_with(&key(), bytes), &public()).is_err(), "{}", String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn the_installer_must_be_the_signed_one() {
        let bytes = manifest_for(b"installer", "2.1.0");
        let manifest = verify_manifest(&bytes, &sign_with(&key(), &bytes), &public()).unwrap();
        assert!(check_installer(b"installer", &manifest).is_ok());
        assert!(check_installer(b"INSTALLER", &manifest).unwrap_err().to_string().contains("hash"));
        assert!(check_installer(b"installer!", &manifest).unwrap_err().to_string().contains("size"));
        assert!(check_installer(b"", &manifest).is_err());
    }

    #[test]
    fn versions_compare_as_numbers() {
        let v = |text| Version::parse(text).unwrap();
        assert!(v("2.10.0") > v("2.9.9"));
        assert!(v("2.1.0") > v("2.0.99"));
        assert!(v("3.0.0") > v("2.99.99"));
        assert!(v("2.0.1") == v("2.0.1"));
        assert!(v("2.0.1") < v("2.0.2"));
        assert_eq!(v("2.10.3").to_string(), "2.10.3");
    }

    #[test]
    fn only_three_plain_numbers_are_a_version() {
        for text in ["", "2", "2.1", "2.1.0.0", "v2.1.0", "2.1.0-rc1", "2.1.x", "+2.1.0", "-2.1.0", "2..0", " 2.1.0", "2.1.0/", "99999999999.0.0"] {
            assert!(Version::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn urls_come_from_the_version_alone() {
        let source = UpdateSource::github();
        let version = Version::parse("2.1.0").unwrap();
        assert_eq!(source.manifest_url(), "https://github.com/gorka2354/shotik/releases/latest/download/latest.json");
        assert_eq!(source.signature_url(), "https://github.com/gorka2354/shotik/releases/latest/download/latest.json.sig");
        assert_eq!(source.installer_url(version), "https://github.com/gorka2354/shotik/releases/download/v2.1.0/Shotik-Setup-2.1.0.exe");
        assert_eq!(source.release_page(version), "https://github.com/gorka2354/shotik/releases/tag/v2.1.0");
        assert_eq!(source.public_key, UPDATE_PUBLIC_KEY);
    }

    /// Made by installer/sign-update.js. test/unit/sign-update.test.js pins the same bytes on its side, so a format
    /// change in either one fails a test instead of cutting off every installed copy.
    #[test]
    fn a_signature_from_the_release_script_verifies() {
        let manifest = br#"{"version":"2.1.0","sha256":"9c0d294c05fc1d88d698034609bb81c0c69196327594e4c69d2915c80fd9850c","size":9}"#;
        let signature = b"7e2bdb87f6eaf1cac50117912af4ee445a7454d0185ace13769fefbcd5939ecc4232666effcea97994167b068b933cb8e0fc2d122c77aa1adb33c0b94f086208\n";
        assert_eq!(public(), "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c");
        let verified = verify_manifest(manifest, signature, &public()).unwrap();
        assert_eq!(verified.version.to_string(), "2.1.0");
        check_installer(b"installer", &verified).unwrap();
    }

    #[test]
    fn a_check_is_due_once_a_day_and_after_the_clock_moves_back() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert!(check_is_due(None, now));
        assert!(!check_is_due(Some(now), now));
        assert!(!check_is_due(Some(now - CHECK_INTERVAL + Duration::from_secs(1)), now));
        assert!(check_is_due(Some(now - CHECK_INTERVAL), now));
        assert!(check_is_due(Some(now + Duration::from_secs(3600)), now));
    }

    #[test]
    fn the_shipped_public_key_is_a_real_key() {
        assert!(VerifyingKey::from_bytes(&from_hex::<32>(UPDATE_PUBLIC_KEY, "public key").unwrap()).is_ok());
    }
}
