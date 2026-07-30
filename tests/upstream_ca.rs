use chat_responses_codex::state::AppConfig;
use chat_responses_codex::upstream_tls::UpstreamCaConfig;
use chat_responses_codex::util::{build_upstream_http_client, join_upstream_url};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use std::path::Path;

#[path = "common/tls.rs"]
mod tls;

fn generated_ca_pem(name: &str) -> String {
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name.push(DnType::CommonName, name);
    let key_pair = KeyPair::generate().unwrap();
    params.self_signed(&key_pair).unwrap().pem()
}

#[test]
fn no_custom_ca_path_preserves_public_roots_only() {
    let config = UpstreamCaConfig::load(None).unwrap();

    assert!(!config.is_configured());
    assert!(config.is_empty());
}

#[test]
fn loads_every_certificate_from_one_pem_bundle() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = directory.path().join("internal-ca-bundle.pem");
    std::fs::write(
        &bundle,
        format!("{}\n{}", generated_ca_pem("ca-a"), generated_ca_pem("ca-b")),
    )
    .unwrap();

    let config = UpstreamCaConfig::load(Some(&bundle)).unwrap();

    assert!(config.is_configured());
    assert_eq!(config.len(), 2);
}

#[test]
fn loads_crt_and_pem_files_from_a_directory() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("b.pem"), generated_ca_pem("ca-b")).unwrap();
    std::fs::write(directory.path().join("a.crt"), generated_ca_pem("ca-a")).unwrap();
    std::fs::write(directory.path().join("README.md"), "ignored").unwrap();

    let config = UpstreamCaConfig::load(Some(directory.path())).unwrap();

    assert!(config.is_configured());
    assert_eq!(config.len(), 2);
}

#[test]
fn ignores_subdirectories_and_unrelated_extensions() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("nested.pem")).unwrap();
    std::fs::write(directory.path().join("notes.txt"), "not a certificate").unwrap();
    std::fs::write(directory.path().join("root.CRT"), generated_ca_pem("root")).unwrap();

    let config = UpstreamCaConfig::load(Some(directory.path())).unwrap();

    assert_eq!(config.len(), 1);
}

#[test]
fn rejects_a_missing_configured_path() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.pem");

    let error = load_error(&missing);

    assert!(error.contains("missing.pem"));
}

#[test]
fn rejects_an_empty_configured_directory() {
    let directory = tempfile::tempdir().unwrap();

    let error = load_error(directory.path());

    assert!(error.contains("no .crt or .pem certificates"));
}

#[test]
fn rejects_an_invalid_selected_certificate_file() {
    let directory = tempfile::tempdir().unwrap();
    let invalid = directory.path().join("invalid.pem");
    std::fs::write(&invalid, "not a certificate").unwrap();

    let error = load_error(directory.path());

    assert!(error.contains("invalid.pem"));
    assert!(!error.contains("not a certificate"));
}

fn load_error(path: &Path) -> String {
    match UpstreamCaConfig::load(Some(path)) {
        Ok(_) => panic!("expected custom CA loading to fail"),
        Err(error) => error.to_string(),
    }
}

#[tokio::test]
async fn custom_ca_trusts_a_private_ca_tls_upstream() {
    let server = tls::spawn_tls_model_server().await;
    let models_url = join_upstream_url(&server.base_url, "/v1/models");
    let untrusted_client = build_upstream_http_client(&AppConfig::default(), false);
    let untrusted = untrusted_client.get(&models_url).send().await;
    assert!(untrusted.is_err());

    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("internal-ca.crt"), &server.ca_pem).unwrap();
    let config = AppConfig {
        upstream_ca: UpstreamCaConfig::load(Some(directory.path())).unwrap(),
        ..Default::default()
    };
    let trusted_client = build_upstream_http_client(&config, false);

    let response = trusted_client.get(&models_url).send().await.unwrap();
    let payload: serde_json::Value = response.json().await.unwrap();

    assert_eq!(payload["data"][0]["id"], "internal-model");
}
