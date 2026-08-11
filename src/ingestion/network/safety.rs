use super::policy::FetchScope;
use crate::ingestion::IngestionError;
use eal_semantic::canonicalize_url;
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};
use tokio::net::lookup_host;
use url::Url;

pub(super) fn parse_http_url(input: &str) -> Result<Url, IngestionError> {
    let mut url = Url::parse(input.trim()).map_err(|error| {
        IngestionError::new("invalid_url", format!("could not parse URL: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(IngestionError::new(
            "invalid_url",
            "URL scheme must be http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(IngestionError::new(
            "invalid_url",
            "URL user information is not allowed",
        ));
    }
    if url.host_str().is_none() {
        return Err(IngestionError::new("invalid_url", "URL host is required"));
    }
    url.set_fragment(None);
    Ok(url)
}

pub(super) fn prepare_target_url(
    input: &str,
    scope: &FetchScope,
) -> Result<Url, IngestionError> {
    let url = parse_http_url(input)?;
    if !scope.allows_url(&url) {
        return Err(IngestionError::new(
            "out_of_scope",
            format!(
                "host {:?} is outside the registered source scope",
                url.host_str()
            ),
        ));
    }
    Ok(url)
}

pub(super) fn canonical_identity(url: &Url) -> Result<String, IngestionError> {
    canonicalize_url(url.as_str())
        .map(|normalized| normalized.canonical)
        .map_err(|error| IngestionError::new("invalid_url", error.to_string()))
}

pub(super) async fn resolve_public_addresses(
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>, IngestionError> {
    let resolved = lookup_host((host, port)).await.map_err(|error| {
        IngestionError::new("dns_failed", format!("DNS lookup failed: {error}"))
    })?;
    let addresses = resolved
        .filter(|address| is_public_ip(address.ip()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(IngestionError::new(
            "dns_no_public_address",
            "host resolved only to private, local, reserved, or documentation addresses",
        ));
    }
    Ok(addresses)
}

pub fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !matches!(a, 0 | 10 | 127)
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 169 && b == 254)
        && !(a == 172 && (16..=31).contains(&b))
        && !(a == 192 && b == 0 && c == 0)
        && !(a == 192 && b == 0 && c == 2)
        && !(a == 192 && b == 88 && c == 99)
        && !(a == 192 && b == 168)
        && !(a == 198 && matches!(b, 18 | 19))
        && !(a == 198 && b == 51 && c == 100)
        && !(a == 203 && b == 0 && c == 113)
        && a < 224
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = address.segments();
    let first = segments[0];
    let is_global_unicast = first & 0xe000 == 0x2000;
    let is_documentation = first == 0x2001 && segments[1] == 0x0db8;
    is_global_unicast && !is_documentation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_and_reserved_addresses() {
        for address in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.168.1.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
