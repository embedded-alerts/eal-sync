use super::safety::parse_http_url;
use crate::ingestion::IngestionError;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::IpAddr, time::Duration};
use url::Url;

#[derive(Debug, Clone)]
pub struct FetchPolicy {
    pub max_redirects: usize,
    pub max_response_bytes: usize,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub max_concurrency_per_host: usize,
    pub require_content_type: bool,
    pub user_agent: String,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            max_redirects: 5,
            max_response_bytes: 2 * 1024 * 1024,
            request_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(5),
            max_concurrency_per_host: 2,
            require_content_type: true,
            user_agent: "embedded-alerts/0.1 (+https://github.com/embedded-alerts/eal-sync)".into(),
        }
    }
}

impl FetchPolicy {
    pub fn validate(&self) -> Result<(), IngestionError> {
        if self.max_redirects > 20 {
            return Err(IngestionError::new(
                "invalid_policy",
                "max_redirects must not exceed 20",
            ));
        }
        if !(1_024..=32 * 1024 * 1024).contains(&self.max_response_bytes) {
            return Err(IngestionError::new(
                "invalid_policy",
                "max_response_bytes must be between 1024 and 33554432",
            ));
        }
        if self.request_timeout.is_zero() || self.request_timeout > Duration::from_secs(120) {
            return Err(IngestionError::new(
                "invalid_policy",
                "request_timeout must be positive and at most 120 seconds",
            ));
        }
        if self.connect_timeout.is_zero() || self.connect_timeout > self.request_timeout {
            return Err(IngestionError::new(
                "invalid_policy",
                "connect_timeout must be positive and no greater than request_timeout",
            ));
        }
        if !(1..=32).contains(&self.max_concurrency_per_host) {
            return Err(IngestionError::new(
                "invalid_policy",
                "max_concurrency_per_host must be between 1 and 32",
            ));
        }
        if self.user_agent.trim().is_empty() || self.user_agent.len() > 512 {
            return Err(IngestionError::new(
                "invalid_policy",
                "user_agent must contain between 1 and 512 bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchScope {
    pub root_host: String,
    pub include_subdomains: bool,
    pub explicitly_allowed_hosts: BTreeSet<String>,
    pub allowed_path_prefixes: BTreeSet<String>,
    pub require_default_port: bool,
}

impl FetchScope {
    pub fn for_url(url: &str) -> Result<Self, IngestionError> {
        let parsed = parse_http_url(url)?;
        let root_host = normalize_host(
            parsed
                .host_str()
                .ok_or_else(|| IngestionError::new("invalid_url", "URL host is required"))?,
        )?;
        let mut allowed_path_prefixes = BTreeSet::new();
        allowed_path_prefixes.insert(normalize_path_prefix(parsed.path())?);
        Ok(Self {
            root_host,
            include_subdomains: false,
            explicitly_allowed_hosts: BTreeSet::new(),
            allowed_path_prefixes,
            require_default_port: true,
        })
    }

    pub fn allow_host(&mut self, host: impl Into<String>) -> Result<(), IngestionError> {
        let host = host.into();
        let normalized = normalize_host(&host)?;
        self.explicitly_allowed_hosts.insert(normalized);
        Ok(())
    }

    pub fn allow_path_prefix(&mut self, prefix: impl AsRef<str>) -> Result<(), IngestionError> {
        self.allowed_path_prefixes
            .insert(normalize_path_prefix(prefix.as_ref())?);
        Ok(())
    }

    pub fn set_allowed_path_prefixes<I, S>(&mut self, prefixes: I) -> Result<(), IngestionError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut normalized = BTreeSet::new();
        for prefix in prefixes {
            normalized.insert(normalize_path_prefix(prefix.as_ref())?);
        }
        if normalized.is_empty() {
            return Err(IngestionError::new(
                "invalid_path_scope",
                "at least one allowed path prefix is required",
            ));
        }
        self.allowed_path_prefixes = normalized;
        Ok(())
    }

    pub fn allows_url(&self, url: &Url) -> bool {
        let Some(host) = url.host_str().and_then(|host| normalize_host(host).ok()) else {
            return false;
        };
        let host_allowed = host == self.root_host
            || self.explicitly_allowed_hosts.contains(&host)
            || (self.include_subdomains
                && host
                    .strip_suffix(&self.root_host)
                    .is_some_and(|prefix| prefix.ends_with('.') && !prefix.is_empty()));
        if !host_allowed || (self.require_default_port && !uses_default_port(url)) {
            return false;
        }

        self.allowed_path_prefixes
            .iter()
            .any(|prefix| path_matches_prefix(url.path(), prefix))
    }
}

fn normalize_host(host: &str) -> Result<String, IngestionError> {
    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels_are_valid = normalized.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    });
    if normalized.is_empty()
        || normalized.len() > 253
        || !normalized.contains('.')
        || normalized.contains('*')
        || normalized.parse::<IpAddr>().is_ok()
        || !labels_are_valid
    {
        return Err(IngestionError::new(
            "invalid_host",
            "allowed host must be a canonical DNS name, not a wildcard, IP literal, or local name",
        ));
    }
    Ok(normalized)
}

fn normalize_path_prefix(prefix: &str) -> Result<String, IngestionError> {
    let trimmed = prefix.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.is_empty()
        || !trimmed.starts_with('/')
        || trimmed
            .chars()
            .any(|character| matches!(character, '?' | '#' | '\\'))
        || trimmed.chars().any(char::is_control)
        || trimmed.contains("//")
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
    {
        return Err(IngestionError::new(
            "invalid_path_scope",
            "path prefixes must be canonical absolute paths without queries, fragments, traversal, encoded separators, controls, or duplicate slashes",
        ));
    }

    let parsed = Url::parse(&format!("https://scope.invalid{trimmed}")).map_err(|error| {
        IngestionError::new(
            "invalid_path_scope",
            format!("could not parse path prefix: {error}"),
        )
    })?;
    Ok(parsed.path().to_owned())
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    let boundary = prefix.trim_end_matches('/');
    path == boundary
        || path
            .strip_prefix(boundary)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn uses_default_port(url: &Url) -> bool {
    matches!(
        (url.scheme(), url.port()),
        ("http", None | Some(80)) | ("https", None | Some(443))
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionalState {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FetchRequest {
    pub url: String,
    pub scope: FetchScope,
    pub conditional: ConditionalState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_rejects_cross_host_redirects_by_default() {
        let scope = FetchScope::for_url("https://example.com/feed").unwrap();
        assert!(scope.allows_url(&Url::parse("https://example.com/feed").unwrap()));
        assert!(!scope.allows_url(&Url::parse("https://evil.example/feed").unwrap()));
    }

    #[test]
    fn scope_rejects_same_host_paths_outside_the_registered_prefix() {
        let scope = FetchScope::for_url("https://example.com/blog").unwrap();
        assert!(scope.allows_url(&Url::parse("https://example.com/blog/post").unwrap()));
        assert!(!scope.allows_url(&Url::parse("https://example.com/blogger").unwrap()));
        assert!(!scope.allows_url(&Url::parse("https://example.com/admin").unwrap()));
    }

    #[test]
    fn explicit_scope_requires_both_host_and_path_authorization() {
        let mut scope = FetchScope::for_url("https://search.example/api").unwrap();
        scope.allow_host("results.example").unwrap();
        assert!(!scope.allows_url(&Url::parse("https://results.example/r/1").unwrap()));
        scope.allow_path_prefix("/r").unwrap();
        assert!(scope.allows_url(&Url::parse("https://results.example/r/1").unwrap()));
    }

    #[test]
    fn scope_rejects_ip_literals_and_non_default_ports() {
        assert!(FetchScope::for_url("https://1.1.1.1/").is_err());
        let scope = FetchScope::for_url("https://example.com/feed").unwrap();
        assert!(!scope.allows_url(&Url::parse("https://example.com:8443/feed").unwrap()));
        assert!(scope.allows_url(&Url::parse("https://example.com:443/feed").unwrap()));
    }

    #[test]
    fn path_scope_rejects_ambiguous_encodings() {
        let mut scope = FetchScope::for_url("https://example.com/").unwrap();
        assert!(scope.allow_path_prefix("/safe/%2e%2e/admin").is_err());
        assert!(scope.allow_path_prefix("/safe//admin").is_err());
        assert!(scope.allow_path_prefix("/safe?debug=1").is_err());
    }

    #[test]
    fn response_byte_policy_has_safe_default_range() {
        let policy = FetchPolicy {
            max_response_bytes: 1_024,
            ..FetchPolicy::default()
        };
        assert!(policy.validate().is_ok());
    }
}
