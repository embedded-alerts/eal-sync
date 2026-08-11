use super::safety::parse_http_url;
use crate::ingestion::IngestionError;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, time::Duration};
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
            user_agent: "embedded-alerts/0.1 (+https://github.com/embedded-alerts/eal-sync)"
                .into(),
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
}

impl FetchScope {
    pub fn for_url(url: &str) -> Result<Self, IngestionError> {
        let parsed = parse_http_url(url)?;
        let root_host = normalize_host(
            parsed
                .host_str()
                .ok_or_else(|| IngestionError::new("invalid_url", "URL host is required"))?,
        )?;
        Ok(Self {
            root_host,
            include_subdomains: false,
            explicitly_allowed_hosts: BTreeSet::new(),
        })
    }

    pub fn allow_host(&mut self, host: impl Into<String>) -> Result<(), IngestionError> {
        let host = host.into();
        let normalized = normalize_host(&host)?;
        self.explicitly_allowed_hosts.insert(normalized);
        Ok(())
    }

    pub fn allows_url(&self, url: &Url) -> bool {
        let Some(host) = url.host_str().and_then(|host| normalize_host(host).ok()) else {
            return false;
        };
        host == self.root_host
            || self.explicitly_allowed_hosts.contains(&host)
            || (self.include_subdomains
                && host
                    .strip_suffix(&self.root_host)
                    .is_some_and(|prefix| prefix.ends_with('.') && !prefix.is_empty()))
    }
}

fn normalize_host(host: &str) -> Result<String, IngestionError> {
    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty()
        || normalized
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '/' | '?' | '#'))
    {
        return Err(IngestionError::new(
            "invalid_host",
            "allowed host is empty or contains invalid characters",
        ));
    }
    Ok(normalized)
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
        assert!(scope.allows_url(&Url::parse("https://example.com/page").unwrap()));
        assert!(!scope.allows_url(&Url::parse("https://evil.example/page").unwrap()));
    }

    #[test]
    fn explicit_scope_can_allow_a_search_provider_redirect_host() {
        let mut scope = FetchScope::for_url("https://search.example/api").unwrap();
        scope.allow_host("results.example").unwrap();
        assert!(scope.allows_url(&Url::parse("https://results.example/r/1").unwrap()));
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
