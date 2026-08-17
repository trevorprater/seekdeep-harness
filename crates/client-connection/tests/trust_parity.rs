//! Behavioral mirror of loopback classification and the browser trust fence.

use std::collections::HashMap;

use seekdeep_client_connection::{
    assert_trusted_authority, is_loopback_hostname, is_trusted_api_request,
};

fn headers(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn trusted(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| (*entry).to_owned()).collect()
}

#[test]
fn accepts_exact_source_loopback_vocabulary() {
    for hostname in [
        "localhost",
        "[::1]",
        "127.0.0.1",
        "127.8.9.10",
        "127.255.255.255",
    ] {
        assert!(is_loopback_hostname(hostname), "{hostname}");
    }
    for hostname in [
        "remote.localhost",
        "::1",
        "128.0.0.1",
        "127.0.0",
        "127.0.0.256",
        "127.0.0.-1",
    ] {
        assert!(!is_loopback_hostname(hostname), "{hostname}");
    }
}

#[test]
fn markerless_requests_still_pass_the_host_fence() {
    assert!(is_trusted_api_request(
        &headers(&[("host", "127.0.0.1:3080")]),
        &[]
    ));
    assert!(is_trusted_api_request(
        &headers(&[("host", "192.168.1.5:3080")]),
        &trusted(&["192.168.1.5"])
    ));
    assert!(!is_trusted_api_request(
        &headers(&[("host", "192.168.1.5:3080")]),
        &[]
    ));
    assert!(!is_trusted_api_request(
        &headers(&[("host", "harness.example")]),
        &[]
    ));
    assert!(!is_trusted_api_request(&HashMap::new(), &[]));
}

#[test]
fn accepts_every_loopback_authority_spelling() {
    for host in [
        "localhost",
        "localhost:3080",
        "127.0.0.1",
        "127.0.0.1:3080",
        "127.8.9.10:80",
        "[::1]",
        "[::1]:3080",
        "LOCALHOST:3080",
    ] {
        assert!(is_trusted_api_request(
            &headers(&[("host", host), ("origin", &format!("http://{host}"))]),
            &[]
        ));
    }
}

#[test]
fn declared_authority_is_exact_with_port_and_wildcard_without_it() {
    let request = headers(&[
        ("host", "harness.internal:3080"),
        ("origin", "http://harness.internal:3080"),
    ]);
    assert!(is_trusted_api_request(
        &request,
        &trusted(&["harness.internal:3080"])
    ));
    assert!(is_trusted_api_request(
        &request,
        &trusted(&["harness.internal"])
    ));
    assert!(!is_trusted_api_request(
        &request,
        &trusted(&["harness.internal:9999"])
    ));
}

#[test]
fn whatwg_case_and_default_port_normalization_match_source() {
    assert!(is_trusted_api_request(
        &headers(&[
            ("host", "Harness.INTERNAL:3080"),
            ("origin", "http://harness.internal:3080"),
        ]),
        &trusted(&["harness.internal:3080"])
    ));
    assert!(is_trusted_api_request(
        &headers(&[
            ("host", "harness.internal"),
            ("origin", "http://harness.internal"),
        ]),
        &trusted(&["HARNESS.internal:80"])
    ));
    assert!(is_trusted_api_request(
        &headers(&[
            ("host", "harness.internal"),
            ("origin", "http://harness.internal"),
        ]),
        &trusted(&["bad entry", "harness.internal"])
    ));
}

#[test]
fn rejects_cross_site_and_opaque_origins() {
    assert!(!is_trusted_api_request(
        &headers(&[
            ("host", "127.0.0.1:3080"),
            ("origin", "http://evil.example"),
        ]),
        &[]
    ));
    assert!(!is_trusted_api_request(
        &headers(&[("host", "127.0.0.1:3080"), ("sec-fetch-site", "cross-site"),]),
        &[]
    ));
    assert!(!is_trusted_api_request(
        &headers(&[("host", "127.0.0.1:3080"), ("origin", "null")]),
        &[]
    ));
}

#[test]
fn validates_only_canonical_bare_trusted_authorities() {
    for entry in [
        "harness.internal",
        "harness.internal:3080",
        "HARNESS.internal:80",
        "10.0.0.9",
        "[::1]:3080",
    ] {
        assert!(assert_trusted_authority(entry).is_ok(), "{entry}");
    }
    for entry in [
        "harness.internal/path",
        "harness.internal/",
        "user@harness.internal",
        "harness.internal?x",
        "harness.internal#f",
        "harness.internal\\path",
        "bad entry",
        "",
        "harness.internal:3080 ",
        " harness.internal",
        "harness.internal:30\t80",
        "harness.internal:",
        "[::1]:",
        "harness.internal:0080",
        "0x7f.0.0.1",
        "[0:0:0:0:0:0:0:1]",
    ] {
        let error = assert_trusted_authority(entry).unwrap_err().to_string();
        assert!(
            error.contains("not a bare host[:port] authority"),
            "{entry}: {error}"
        );
    }
}

#[test]
fn malformed_trimmed_entry_never_broadens_an_exact_port_grant() {
    let malformed = trusted(&["harness.internal:3080 "]);
    assert!(!is_trusted_api_request(
        &headers(&[
            ("host", "harness.internal:9999"),
            ("origin", "http://harness.internal:9999"),
        ]),
        &malformed
    ));
    assert!(is_trusted_api_request(
        &headers(&[
            ("host", "harness.internal:3080"),
            ("origin", "http://harness.internal:3080"),
        ]),
        &malformed
    ));
}

#[test]
fn malformed_and_untrusted_request_authorities_fail_closed() {
    for host in [
        None,
        Some(""),
        Some("bad host"),
        Some("127.0.0.999"),
        Some("128.0.0.1"),
    ] {
        let mut request = headers(&[("sec-fetch-site", "same-origin")]);
        if let Some(host) = host {
            request.insert("host".to_owned(), host.to_owned());
        }
        assert!(!is_trusted_api_request(&request, &[]), "{host:?}");
    }
}
