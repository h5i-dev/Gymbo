//! Verifying that a download is what the repository says it is.
//!
//! Maven publishes a `.sha1` beside every artifact, and newer repositories add
//! `.sha256` and `.sha512`. jv checks the strongest one it can get, because the
//! whole point of a checksum here is supply-chain integrity rather than
//! detecting a truncated transfer.
//!
//! Published checksum files are not always clean: some carry the file name after
//! the digest, some have trailing whitespace, and a few older ones are
//! upper-case. Parsing tolerates all three, because rejecting an artifact over
//! formatting would be a false alarm about the thing checksums exist to prevent.

use jv_repo::Checksum;
use md5::Md5;
use sha1::{Digest, Sha1};
use sha2::{Sha256, Sha512};

/// A checksum did not match, or could not be read.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChecksumError {
    #[error(
        "{algorithm} mismatch: the repository published {expected}, the bytes hash to {actual}"
    )]
    Mismatch {
        algorithm: &'static str,
        expected: String,
        actual: String,
    },
    #[error("the published {algorithm} checksum is not a {length}-character hex digest: {text:?}")]
    Malformed {
        algorithm: &'static str,
        length: usize,
        text: String,
    },
}

/// The digest of some bytes, lower-case hex.
pub fn digest(bytes: &[u8], checksum: Checksum) -> String {
    match checksum {
        Checksum::Sha1 => hex::encode(Sha1::digest(bytes)),
        Checksum::Sha256 => hex::encode(Sha256::digest(bytes)),
        Checksum::Sha512 => hex::encode(Sha512::digest(bytes)),
        Checksum::Md5 => hex::encode(Md5::digest(bytes)),
    }
}

/// Reads a published checksum file.
///
/// Takes the first whitespace-delimited token, so a `sha1sum`-style line with
/// the file name after the digest is read correctly, and lower-cases it.
///
/// # Examples
///
/// ```
/// use jv_cache::parse_published;
/// use jv_repo::Checksum;
///
/// let plain = "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3";
/// assert_eq!(parse_published(plain, Checksum::Sha1).unwrap(), plain);
///
/// // sha1sum output, and upper case, both read the same.
/// let annotated = "A94A8FE5CCB19BA61C4C0873D391E987982FBBD3  test-1.0.jar\n";
/// assert_eq!(parse_published(annotated, Checksum::Sha1).unwrap(), plain);
/// ```
pub fn parse_published(text: &str, checksum: Checksum) -> Result<String, ChecksumError> {
    let candidate = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let expected_length = checksum.hex_len();
    if candidate.len() != expected_length || !candidate.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ChecksumError::Malformed {
            algorithm: checksum.extension(),
            length: expected_length,
            text: text.trim().to_owned(),
        });
    }
    Ok(candidate)
}

/// Checks bytes against a published checksum file's contents.
pub fn verify(bytes: &[u8], published: &str, checksum: Checksum) -> Result<(), ChecksumError> {
    let expected = parse_published(published, checksum)?;
    let actual = digest(bytes, checksum);
    if actual == expected {
        return Ok(());
    }
    Err(ChecksumError::Mismatch {
        algorithm: checksum.extension(),
        expected,
        actual,
    })
}

/// The algorithms to try, strongest first.
///
/// Repositories are inconsistent about which they publish, so the fetcher walks
/// this list and verifies against the first one it can retrieve.
pub const PREFERRED: &[Checksum] = &[Checksum::Sha512, Checksum::Sha256, Checksum::Sha1];

#[cfg(test)]
mod tests {
    use super::*;

    /// `sha1("abc")`, and friends.
    const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const ABC_MD5: &str = "900150983cd24fb0d6963f7d28e17f72";

    #[test]
    fn digests_match_the_known_vectors() {
        assert_eq!(digest(b"abc", Checksum::Sha1), ABC_SHA1);
        assert_eq!(digest(b"abc", Checksum::Sha256), ABC_SHA256);
        assert_eq!(digest(b"abc", Checksum::Md5), ABC_MD5);
        assert_eq!(digest(b"abc", Checksum::Sha512).len(), 128);
    }

    #[test]
    fn an_empty_input_still_hashes() {
        // A zero-length artifact is unusual but not an error, and it must not
        // hash to the empty string.
        assert_eq!(digest(b"", Checksum::Sha1).len(), 40);
    }

    #[test]
    fn published_files_are_read_tolerantly() {
        assert_eq!(parse_published(ABC_SHA1, Checksum::Sha1).unwrap(), ABC_SHA1);
        assert_eq!(
            parse_published(&format!("{ABC_SHA1}\n"), Checksum::Sha1).unwrap(),
            ABC_SHA1
        );
        assert_eq!(
            parse_published(&format!("{ABC_SHA1}  artifact.jar\n"), Checksum::Sha1).unwrap(),
            ABC_SHA1
        );
        assert_eq!(
            parse_published(&ABC_SHA1.to_uppercase(), Checksum::Sha1).unwrap(),
            ABC_SHA1
        );
    }

    #[test]
    fn a_wrong_length_digest_is_malformed() {
        // The commonest real failure: a proxy served an HTML error page.
        let html = "<!DOCTYPE html><html><body>404</body></html>";
        assert!(matches!(
            parse_published(html, Checksum::Sha1),
            Err(ChecksumError::Malformed { .. })
        ));
        // And a sha1 read as a sha256 is caught by the length.
        assert!(parse_published(ABC_SHA1, Checksum::Sha256).is_err());
    }

    #[test]
    fn non_hex_characters_are_malformed() {
        let wrong = "z9993e364706816aba3e25717850c26c9cd0d89d";
        assert!(parse_published(wrong, Checksum::Sha1).is_err());
    }

    #[test]
    fn verification_accepts_and_rejects() {
        assert!(verify(b"abc", ABC_SHA1, Checksum::Sha1).is_ok());
        assert!(verify(b"abc", &format!("{ABC_SHA1}  x.jar"), Checksum::Sha1).is_ok());

        let error = verify(b"abd", ABC_SHA1, Checksum::Sha1).unwrap_err();
        match error {
            ChecksumError::Mismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, ABC_SHA1);
                assert_ne!(actual, ABC_SHA1);
            }
            other => panic!("expected a mismatch, got {other:?}"),
        }
    }

    #[test]
    fn the_error_names_both_digests() {
        // A mismatch report that omits the actual digest cannot be acted on.
        let message = verify(b"abd", ABC_SHA1, Checksum::Sha1)
            .unwrap_err()
            .to_string();
        assert!(message.contains(ABC_SHA1));
        assert!(message.contains(&digest(b"abd", Checksum::Sha1)));
    }

    #[test]
    fn the_preferred_order_is_strongest_first() {
        assert_eq!(PREFERRED[0], Checksum::Sha512);
        assert_eq!(PREFERRED[PREFERRED.len() - 1], Checksum::Sha1);
        // md5 is not offered: it is published by old repositories but is not
        // worth verifying against when anything else is available.
        assert!(!PREFERRED.contains(&Checksum::Md5));
    }
}
