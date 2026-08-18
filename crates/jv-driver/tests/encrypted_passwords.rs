//! An encrypted `<server>` password becomes a usable credential.
//!
//! The unit tests in `jv_model::security` prove the cipher matches Maven's,
//! using ciphertext Maven itself produced. This proves the other half: that the
//! decrypted value actually reaches the code that authenticates, rather than
//! being decrypted into a value nothing reads.
//!
//! That distinction is the whole bug class — `<proxies>` parsed fine for months
//! and was never applied — so it is worth a test that goes through `Config`
//! rather than calling the cipher directly.

use std::path::PathBuf;

use jv_driver::Config;

/// Produced by `mvn --encrypt-master-password correct-horse` on Maven 3.9.9.
const MASTER_TOKEN: &str = "{Oy24Ys7RpF8HJdlKpH5gZltn2wAif2YTvptbJdGoXM4=}";
/// Produced by `mvn --encrypt-password s3cr3t-pa55` against that master.
const SERVER_TOKEN: &str = "{YDW2WjAyaqwHRJiwp07FyRz4FpDiV2kVXjqqWcbMnu8=}";

struct Fixture {
    _directory: tempfile::TempDir,
    settings: PathBuf,
    security: PathBuf,
}

fn fixture(master: Option<&str>) -> Fixture {
    let directory = tempfile::tempdir().expect("a temp dir");
    let settings = directory.path().join("settings.xml");
    std::fs::write(
        &settings,
        format!(
            r#"<settings>
  <servers>
    <server>
      <id>corporate</id>
      <username>build</username>
      <password>{SERVER_TOKEN}</password>
    </server>
    <server>
      <id>plain</id>
      <username>build</username>
      <password>literal-password</password>
    </server>
  </servers>
</settings>"#
        ),
    )
    .unwrap();

    let security = directory.path().join("settings-security.xml");
    if let Some(master) = master {
        std::fs::write(
            &security,
            format!("<settingsSecurity><master>{master}</master></settingsSecurity>"),
        )
        .unwrap();
    }

    Fixture {
        _directory: directory,
        settings,
        security,
    }
}

fn config(fixture: &Fixture, with_security: bool) -> Config {
    Config {
        user_settings: Some(fixture.settings.clone()),
        // Pinned so a developer's own `~/.m2/settings-security.xml` cannot
        // change the result of this test.
        settings_security: Some(if with_security {
            fixture.security.clone()
        } else {
            fixture.settings.with_file_name("absent-security.xml")
        }),
        ..Config::default()
    }
}

#[test]
fn an_encrypted_password_is_decrypted_when_the_master_is_available() {
    let fixture = fixture(Some(MASTER_TOKEN));
    let settings = config(&fixture, true)
        .load_settings()
        .expect("settings load");

    let server = settings.server("corporate").expect("the corporate server");
    assert_eq!(
        server.password.as_deref(),
        Some("s3cr3t-pa55"),
        "the encrypted password did not reach the settings as plaintext"
    );
    assert!(
        !server.has_encrypted_password(),
        "it should no longer look encrypted, so the withholding path does not fire"
    );
}

#[test]
fn a_plain_password_beside_an_encrypted_one_is_untouched() {
    let fixture = fixture(Some(MASTER_TOKEN));
    let settings = config(&fixture, true)
        .load_settings()
        .expect("settings load");
    assert_eq!(
        settings.server("plain").unwrap().password.as_deref(),
        Some("literal-password")
    );
}

#[test]
fn without_a_security_file_the_ciphertext_is_left_alone() {
    // Left encrypted rather than mangled, so `resolve_with_trust` still
    // withholds it instead of sending ciphertext as a password.
    let fixture = fixture(None);
    let settings = config(&fixture, false)
        .load_settings()
        .expect("settings should still load");

    let server = settings.server("corporate").expect("the corporate server");
    assert_eq!(server.password.as_deref(), Some(SERVER_TOKEN));
    assert!(server.has_encrypted_password());
}

#[test]
fn a_wrong_master_leaves_the_value_encrypted_rather_than_guessing() {
    // A master that decrypts to something else entirely: the server password
    // must not be replaced by garbage.
    let fixture = fixture(Some("{DPpJwp4qPWYHNoIekCNRrb1+rwx40wJwJE0NgbwZM0o=}"));
    let settings = config(&fixture, true)
        .load_settings()
        .expect("settings load");
    assert_eq!(
        settings.server("corporate").unwrap().password.as_deref(),
        Some(SERVER_TOKEN)
    );
}

#[test]
fn a_relocation_is_followed_to_the_real_master() {
    let fixture = fixture(Some(MASTER_TOKEN));
    let pointer = fixture.security.with_file_name("pointer-security.xml");
    std::fs::write(
        &pointer,
        format!(
            "<settingsSecurity><relocation>{}</relocation></settingsSecurity>",
            fixture.security.display()
        ),
    )
    .unwrap();

    let settings = Config {
        user_settings: Some(fixture.settings.clone()),
        settings_security: Some(pointer),
        ..Config::default()
    }
    .load_settings()
    .expect("settings load");

    assert_eq!(
        settings.server("corporate").unwrap().password.as_deref(),
        Some("s3cr3t-pa55")
    );
}
