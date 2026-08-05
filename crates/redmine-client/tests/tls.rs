//! TLS/certificate configuration: `danger_accept_invalid_certs` logs a WARN,
//! a malformed CA/identity is a build-time `Error::Config` (never a silent
//! fallback to default roots), and a well-formed CA/identity is accepted.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use redmine_client::{Error, RedmineClientBuilder};

const CA_PEM: &str = include_str!("certs/ca.pem");
const CLIENT_IDENTITY_PEM: &str = include_str!("certs/client_identity.pem");

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn danger_accept_invalid_certs_logs_a_warn_unconditionally() {
    let buf = SharedBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_max_level(tracing::Level::WARN)
        .without_time()
        .finish();

    let guard = tracing::subscriber::set_default(subscriber);
    let result = RedmineClientBuilder::new("https://example.com/".parse().unwrap())
        .danger_accept_invalid_certs(true)
        .build();
    assert!(result.is_ok());
    drop(guard);

    let output = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("WARN"),
        "expected a WARN line, got: {output}"
    );
    assert!(
        output.to_ascii_lowercase().contains("certificate"),
        "WARN should mention certificates, got: {output}"
    );
}

#[tokio::test]
async fn malformed_root_certificate_is_rejected_at_the_call_site() {
    let err = RedmineClientBuilder::new("https://example.com/".parse().unwrap())
        .add_root_certificate_pem(b"this is not a PEM certificate")
        .expect_err("bytes with no PEM markers must not silently become 'zero certs added'");
    assert!(matches!(err, Error::Config { .. }));
}

#[tokio::test]
async fn corrupt_pem_payload_is_rejected_not_silently_ignored() {
    // A PEM block with valid markers but a corrupt base64/DER payload is
    // caught the same way: never a silent fallback to the system roots alone.
    let corrupt_pem =
        b"-----BEGIN CERTIFICATE-----\nnot valid base64 der!!\n-----END CERTIFICATE-----\n";
    let err = RedmineClientBuilder::new("https://example.com/".parse().unwrap())
        .add_root_certificate_pem(corrupt_pem)
        .expect_err("a corrupt certificate must not silently become 'no custom CA configured'");
    assert!(matches!(err, Error::Config { .. }));
}

#[tokio::test]
async fn well_formed_root_certificate_is_accepted() {
    let client = RedmineClientBuilder::new("https://example.com/".parse().unwrap())
        .add_root_certificate_pem(CA_PEM.as_bytes())
        .expect("a valid PEM certificate must be accepted")
        .build();
    assert!(client.is_ok());
}

#[tokio::test]
async fn malformed_client_identity_is_rejected_not_silently_ignored() {
    let err = RedmineClientBuilder::new("https://example.com/".parse().unwrap())
        .client_identity_pem(b"this is not a PEM identity")
        .expect_err("garbage bytes must not silently become 'no client cert configured'");
    assert!(matches!(err, Error::Config { .. }));
}

#[tokio::test]
async fn well_formed_client_identity_is_accepted() {
    let client = RedmineClientBuilder::new("https://example.com/".parse().unwrap())
        .client_identity_pem(CLIENT_IDENTITY_PEM.as_bytes())
        .expect("a valid PEM cert+key must be accepted")
        .build();
    assert!(client.is_ok());
}
