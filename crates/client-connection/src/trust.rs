//! Browser request authority, DNS-rebinding, and cross-site trust fence.

use std::{collections::HashMap, hash::BuildHasher};

use url::{Host, Url};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Authority {
    hostname: String,
    port: Option<u16>,
}

impl Authority {
    fn host(&self) -> String {
        self.port.map_or_else(
            || self.hostname.clone(),
            |port| format!("{}:{port}", self.hostname),
        )
    }
}

/// Whether a normalized URL hostname names the local loopback authority.
#[must_use]
pub fn is_loopback_hostname(hostname: &str) -> bool {
    if hostname == "localhost" || hostname == "[::1]" {
        return true;
    }
    let parts = hostname.split('.').collect::<Vec<_>>();
    parts.len() == 4
        && parts[0] == "127"
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u16>().is_ok_and(|value| value <= 255)
        })
}

fn normalized_hostname(host: &Host<&str>) -> String {
    match host {
        Host::Domain(domain) => domain.to_ascii_lowercase(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => format!("[{address}]"),
    }
}

fn parse_authority(authority: &str) -> Option<Authority> {
    let url = Url::parse(&format!("http://{authority}")).ok()?;
    let hostname = normalized_hostname(&url.host()?);
    Some(Authority {
        hostname,
        port: url.port(),
    })
}

fn explicit_port(entry: &str) -> Option<u16> {
    let http = Url::parse(&format!("http://{entry}")).ok()?;
    if let Some(port) = http.port() {
        return Some(port);
    }
    Url::parse(&format!("https://{entry}")).ok()?.port()
}

fn canonical_authority(entry: &str, authority: &Authority) -> String {
    explicit_port(entry).map_or_else(
        || authority.hostname.clone(),
        |port| format!("{}:{port}", authority.hostname),
    )
}

/// Validates one configured trusted host as a canonical bare `host[:port]` authority.
///
/// # Errors
///
/// Returns a load-time error when URL parsing would trim, rewrite, or reinterpret the entry.
pub fn assert_trusted_authority(entry: &str) -> anyhow::Result<()> {
    let valid = parse_authority(entry).is_some_and(|authority| {
        canonical_authority(entry, &authority) == entry.to_ascii_lowercase()
    });
    anyhow::ensure!(
        valid,
        "client-connection: trustedHosts entry {entry:?} is not a bare host[:port] authority"
    );
    Ok(())
}

fn is_trusted_authority(host: &Authority, trusted_hosts: &[String]) -> bool {
    trusted_hosts.iter().any(|entry| {
        let Some(candidate) = parse_authority(entry) else {
            return false;
        };
        if canonical_authority(entry, &candidate) == candidate.hostname {
            candidate.hostname == host.hostname
        } else {
            candidate == *host
        }
    })
}

fn header<'a, S: BuildHasher>(
    headers: &'a HashMap<String, String, S>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Decides whether a request passes the Host, Fetch-Metadata, and Origin trust fence.
#[must_use]
pub fn is_trusted_api_request<S: BuildHasher>(
    headers: &HashMap<String, String, S>,
    trusted_hosts: &[String],
) -> bool {
    let Some(host) = header(headers, "host").and_then(parse_authority) else {
        return false;
    };
    if !is_loopback_hostname(&host.hostname) && !is_trusted_authority(&host, trusted_hosts) {
        return false;
    }
    if header(headers, "sec-fetch-site") == Some("cross-site") {
        return false;
    }
    let Some(origin) = header(headers, "origin") else {
        return true;
    };
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    let Some(origin_host) = url.host().map(|host| normalized_hostname(&host)) else {
        return false;
    };
    let origin = Authority {
        hostname: origin_host,
        port: url.port(),
    };
    origin.host() == host.host()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn rust_url_keeps_whatwg_ipv4_normalization_visible() {
        assert_eq!(
            parse_authority("0x7f.0.0.1").unwrap().hostname,
            Ipv4Addr::LOCALHOST.to_string()
        );
    }
}
