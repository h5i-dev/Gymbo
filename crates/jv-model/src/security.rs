//! Decrypting `settings.xml` passwords with `settings-security.xml`.
//!
//! Maven lets a `<server>` carry `<password>{base64}</password>` instead of a
//! literal, decrypted with a master password held in `~/.m2/settings-security.xml`
//! — which is itself encrypted, with the fixed passphrase `settings.security`.
//! Without this, jv can parse an enterprise `settings.xml` and still fail to
//! authenticate to the only repository the build needs, which is a large share
//! of corporate Java setups.
//!
//! # Where the algorithm came from
//!
//! Not from memory. The constants were read out of the `plexus-cipher-2.0.jar`
//! that Maven 3.9.9 ships (`SHA-256`, `AES/CBC/PKCS5Padding`), and the
//! implementation is checked against ciphertexts produced by that same Maven
//! via `mvn --encrypt-master-password` / `--encrypt-password` — see the tests.
//! Guessing at a cipher and finding out in the field is not an option here: the
//! failure mode is a password that decrypts to plausible garbage and gets sent
//! to a server.
//!
//! # The scheme
//!
//! The decorated form is `{` + base64 + `}`. Decoded, the blob is:
//!
//! ```text
//! | salt (8 bytes) | padLen (1 byte) | ciphertext | random padding (padLen) |
//! ```
//!
//! The key and IV both come from a single SHA-256 over the password followed by
//! the salt: the digest is 32 bytes, exactly key (16) ‖ IV (16), so the
//! OpenSSL-style extension loop in the Java runs only one round. AES-128-CBC
//! with PKCS#7 padding then gives the plaintext.

use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine as _;
use sha2::{Digest, Sha256};

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// The passphrase the master password itself is encrypted with.
///
/// A fixed string in `DefaultSecDispatcher`, so the master password in
/// `settings-security.xml` is obfuscated rather than secret. Recorded here so
/// nobody mistakes this file for a security boundary.
const MASTER_PASSPHRASE: &str = "settings.security";

const SALT_LEN: usize = 8;

/// A password could not be decrypted.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecurityError {
    #[error("{0} is not an encrypted value; expected {{base64}}")]
    NotEncrypted(String),
    #[error("the encrypted value is not valid base64: {0}")]
    Base64(String),
    #[error("the encrypted value is too short to contain a salt and a length")]
    Truncated,
    #[error("the encrypted value's declared padding is longer than the value itself")]
    BadPadding,
    #[error(
        "decryption failed; the master password in settings-security.xml does not match this value"
    )]
    WrongPassword,
    #[error("the decrypted password is not valid UTF-8")]
    NotUtf8,
    #[error("settings-security.xml has no <master>")]
    NoMaster,
}

/// Whether a value is in the `{...}` encrypted form.
///
/// Mirrors `DefaultPlexusCipher.isEncryptedString`, which looks for a `{` that
/// is not backslash-escaped followed by a `}` that is not either. A literal
/// password containing braces is therefore only mistaken for ciphertext if it
/// also happens to be valid base64, which decryption then rejects.
pub fn is_encrypted(value: &str) -> bool {
    inner(value).is_some()
}

/// The base64 payload inside `{...}`, if there is one.
fn inner(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    let mut start = None;
    for (index, byte) in bytes.iter().enumerate() {
        let escaped = index > 0 && bytes[index - 1] == b'\\';
        match byte {
            b'{' if !escaped && start.is_none() => start = Some(index + 1),
            b'}' if !escaped => {
                if let Some(from) = start
                    && index > from
                {
                    return Some(&value[from..index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Decrypts a `{...}` value with a password.
pub fn decrypt(value: &str, password: &str) -> Result<String, SecurityError> {
    let payload = inner(value).ok_or_else(|| SecurityError::NotEncrypted(value.to_owned()))?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|error| SecurityError::Base64(error.to_string()))?;

    if blob.len() < SALT_LEN + 1 {
        return Err(SecurityError::Truncated);
    }
    let salt = &blob[..SALT_LEN];
    let pad_len = blob[SALT_LEN] as usize;
    let ciphertext_end = blob
        .len()
        .checked_sub(pad_len)
        .ok_or(SecurityError::BadPadding)?;
    if ciphertext_end <= SALT_LEN + 1 {
        return Err(SecurityError::BadPadding);
    }
    let ciphertext = &blob[SALT_LEN + 1..ciphertext_end];

    // key ‖ iv = SHA-256(password ‖ salt). The Java extends the digest in a
    // loop for shorter hashes; with SHA-256 the first round already fills both.
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hasher.update(salt);
    let key_and_iv = hasher.finalize();
    let (key, iv) = key_and_iv.split_at(16);

    let plaintext = Aes128CbcDec::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        // A wrong master password almost always shows up as a padding error,
        // because PKCS#7 on random plaintext is overwhelmingly invalid.
        .map_err(|_| SecurityError::WrongPassword)?;

    String::from_utf8(plaintext).map_err(|_| SecurityError::NotUtf8)
}

/// The contents of a `settings-security.xml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsSecurity {
    /// The encrypted master password.
    pub master: Option<String>,
    /// Points at another `settings-security.xml` to read instead.
    pub relocation: Option<String>,
}

impl SettingsSecurity {
    /// The decrypted master password.
    pub fn master_password(&self) -> Result<String, SecurityError> {
        let master = self.master.as_deref().ok_or(SecurityError::NoMaster)?;
        decrypt(master, MASTER_PASSPHRASE)
    }
}

/// Reads a `settings-security.xml`.
///
/// Deliberately tolerant: this file is small and hand-written, and refusing a
/// build over an unexpected element in it would be worse than ignoring one.
pub fn parse_settings_security(xml: &str) -> SettingsSecurity {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut security = SettingsSecurity::default();
    let mut path: Vec<String> = Vec::new();
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(element)) => {
                path.push(String::from_utf8_lossy(element.local_name().as_ref()).into_owned());
            }
            Ok(quick_xml::events::Event::End(_)) => {
                path.pop();
            }
            Ok(quick_xml::events::Event::Text(text)) => {
                let value = text.unescape().unwrap_or_default().trim().to_string();
                if value.is_empty() {
                    continue;
                }
                match path.as_slice() {
                    [root, field] if root == "settingsSecurity" && field == "master" => {
                        security.master = Some(value);
                    }
                    [root, field] if root == "settingsSecurity" && field == "relocation" => {
                        security.relocation = Some(value);
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
    security
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every ciphertext below was produced by the Maven 3.9.9 this project tests
    // against, via `mvn --encrypt-master-password` and `--encrypt-password`.
    // They are the reason this file is not a guess.
    const MASTER_TOKEN: &str = "{Oy24Ys7RpF8HJdlKpH5gZltn2wAif2YTvptbJdGoXM4=}";
    const MASTER_PLAINTEXT: &str = "correct-horse";
    const SERVER_TOKEN: &str = "{YDW2WjAyaqwHRJiwp07FyRz4FpDiV2kVXjqqWcbMnu8=}";
    const SERVER_PLAINTEXT: &str = "s3cr3t-pa55";

    #[test]
    fn the_master_password_decrypts_with_mavens_fixed_passphrase() {
        assert_eq!(
            decrypt(MASTER_TOKEN, MASTER_PASSPHRASE).unwrap(),
            MASTER_PLAINTEXT
        );
    }

    #[test]
    fn a_server_password_decrypts_with_the_master() {
        assert_eq!(
            decrypt(SERVER_TOKEN, MASTER_PLAINTEXT).unwrap(),
            SERVER_PLAINTEXT
        );
    }

    #[test]
    fn a_second_encryption_of_the_same_password_also_decrypts() {
        // Different salt, same plaintext — proves the salt is read from the
        // blob rather than assumed.
        assert_eq!(
            decrypt(
                "{DPpJwp4qPWYHNoIekCNRrb1+rwx40wJwJE0NgbwZM0o=}",
                MASTER_PLAINTEXT
            )
            .unwrap(),
            SERVER_PLAINTEXT
        );
    }

    #[test]
    fn non_ascii_and_punctuation_survive_the_round_trip() {
        // UTF-8 and the characters that would break a naive userinfo or XML
        // handling on the way in.
        assert_eq!(
            decrypt(
                "{aqqLVpDTqQEH8cZZF0syYyGsHH+bNAyUGQp7avMtnxxD/MQvMtc3ZoIveg7awxUt}",
                MASTER_PLAINTEXT
            )
            .unwrap(),
            "pä$$:word@日本"
        );
    }

    #[test]
    fn a_wrong_master_is_reported_rather_than_returning_garbage() {
        assert_eq!(
            decrypt(SERVER_TOKEN, "not-the-master"),
            Err(SecurityError::WrongPassword)
        );
    }

    #[test]
    fn plain_passwords_are_not_mistaken_for_ciphertext() {
        assert!(!is_encrypted("hunter2"));
        assert!(!is_encrypted(""));
        // Escaped braces are a literal password in Maven's grammar.
        assert!(!is_encrypted("\\{not-encrypted\\}"));
        assert!(is_encrypted(SERVER_TOKEN));
        assert!(is_encrypted("  {abc}  "));
    }

    #[test]
    fn an_empty_decoration_is_not_ciphertext() {
        assert!(!is_encrypted("{}"));
    }

    #[test]
    fn the_security_file_parses() {
        let security = parse_settings_security(&format!(
            "<settingsSecurity>\n  <master>{MASTER_TOKEN}</master>\n</settingsSecurity>"
        ));
        assert_eq!(security.master.as_deref(), Some(MASTER_TOKEN));
        assert_eq!(security.master_password().unwrap(), MASTER_PLAINTEXT);
    }

    #[test]
    fn a_relocation_is_read() {
        let security = parse_settings_security(
            "<settingsSecurity><relocation>/etc/jv/sec.xml</relocation></settingsSecurity>",
        );
        assert_eq!(security.relocation.as_deref(), Some("/etc/jv/sec.xml"));
        assert_eq!(security.master_password(), Err(SecurityError::NoMaster));
    }

    #[test]
    fn a_truncated_value_is_rejected_rather_than_panicking() {
        assert_eq!(decrypt("{YWJj}", "x"), Err(SecurityError::Truncated));
        assert!(matches!(
            decrypt("{not base64!!}", "x"),
            Err(SecurityError::Base64(_))
        ));
        // padLen larger than the blob: the subtraction must not wrap.
        let blob = base64::engine::general_purpose::STANDARD.encode(
            [0u8; 8]
                .iter()
                .chain([255u8].iter())
                .copied()
                .collect::<Vec<u8>>(),
        );
        assert_eq!(
            decrypt(&format!("{{{blob}}}"), "x"),
            Err(SecurityError::BadPadding)
        );
    }
}
