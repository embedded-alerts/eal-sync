use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CrawlJob {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub source_id: Uuid,
    pub start_url: String,
    pub interval_seconds: i64,
    pub attempt_count: i32,
    pub max_attempts: i32,
    pub lease_token: Uuid,
    pub attempt_id: Uuid,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrawlCommandRequest {
    pub protocol_version: &'static str,
    pub job_id: Uuid,
    pub attempt_id: Uuid,
    pub tenant_id: Uuid,
    pub source_id: Uuid,
    pub start_url: String,
    pub leased_at: DateTime<Utc>,
}

impl From<&CrawlJob> for CrawlCommandRequest {
    fn from(job: &CrawlJob) -> Self {
        Self {
            protocol_version: "eal-crawl-command/v1",
            job_id: job.id,
            attempt_id: job.attempt_id,
            tenant_id: job.tenant_id,
            source_id: job.source_id,
            start_url: job.start_url.clone(),
            leased_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CrawlCommandConfig {
    pub executable: PathBuf,
    pub timeout_seconds: u64,
    pub stdout_limit_bytes: usize,
    pub stderr_limit_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct CrawlCommandOutput {
    pub page_ingest: Value,
    pub diagnostic: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrawlCommandEnvelope {
    pub protocol_version: String,
    pub page_ingest: Value,
    #[serde(default)]
    pub diagnostic: Value,
}

impl CrawlCommandEnvelope {
    pub fn validate(self, job: &CrawlJob) -> Result<CrawlCommandOutput> {
        if self.protocol_version != "eal-crawl-result/v1" {
            bail!("crawl command returned an unsupported protocol version");
        }
        let page = self
            .page_ingest
            .as_object()
            .context("page_ingest must be a JSON object")?;
        let source_id = page
            .get("source_id")
            .and_then(Value::as_str)
            .context("page_ingest.source_id is required")?
            .parse::<Uuid>()
            .context("page_ingest.source_id must be a UUID")?;
        if source_id != job.source_id {
            bail!("crawl command source_id does not match the leased job");
        }
        let canonical_url = page
            .get("canonical_url")
            .and_then(Value::as_str)
            .context("page_ingest.canonical_url is required")?;
        validate_public_https_url(canonical_url)?;
        let content_hash = page
            .get("content_hash")
            .and_then(Value::as_str)
            .context("page_ingest.content_hash is required")?;
        if content_hash.len() < 32 || content_hash.len() > 256 {
            bail!("page_ingest.content_hash has an invalid length");
        }
        let embedding = page
            .get("embedding")
            .and_then(Value::as_object)
            .context("page_ingest.embedding is required")?;
        for key in [
            "model",
            "model_version",
            "dimensions",
            "normalization",
            "values",
        ] {
            if !embedding.contains_key(key) {
                bail!("page_ingest.embedding.{key} is required");
            }
        }
        let dimensions = embedding
            .get("dimensions")
            .and_then(Value::as_u64)
            .context("page_ingest.embedding.dimensions must be an integer")?;
        let values = embedding
            .get("values")
            .and_then(Value::as_array)
            .context("page_ingest.embedding.values must be an array")?;
        if dimensions == 0 || dimensions > 65_535 || values.len() != dimensions as usize {
            bail!("page_ingest embedding dimensions do not match values");
        }
        if values
            .iter()
            .any(|value| value.as_f64().is_none_or(|number| !number.is_finite()))
        {
            bail!("page_ingest embedding contains a non-finite value");
        }
        Ok(CrawlCommandOutput {
            page_ingest: self.page_ingest,
            diagnostic: self.diagnostic,
        })
    }
}

pub fn validate_public_https_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("URL must be absolute")?;
    if url.scheme() != "https" {
        bail!("crawl URLs must use HTTPS");
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        bail!("crawl URLs must not contain credentials or fragments");
    }
    let host = url.host_str().context("crawl URL must contain a host")?;
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(is_non_public_ip)
    {
        bail!("crawl URL host must be publicly routable");
    }
    Ok(url)
}

fn is_non_public_ip(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => {
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_broadcast()
                || address.is_documentation()
                || address.is_unspecified()
                || address.octets()[0] == 0
                || address.octets()[0] >= 224
        }
        std::net::IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.segments()[0] & 0xffc0 == 0xfec0
                || address.segments()[0] & 0xff00 == 0xff00
                || address.segments()[0] == 0x2001 && address.segments()[1] == 0x0db8
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_start_url() {
        assert!(validate_public_https_url("https://127.0.0.1/private").is_err());
    }

    #[test]
    fn accepts_public_https_hostname() {
        assert!(validate_public_https_url("https://docs.example.com/releases").is_ok());
    }

    #[test]
    fn result_must_match_source_and_embedding_dimensions() {
        let source_id = Uuid::new_v4();
        let job = CrawlJob {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            source_id,
            start_url: "https://docs.example.com".into(),
            interval_seconds: 3600,
            attempt_count: 0,
            max_attempts: 12,
            lease_token: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
        };
        let envelope = CrawlCommandEnvelope {
            protocol_version: "eal-crawl-result/v1".into(),
            page_ingest: serde_json::json!({
                "source_id": source_id,
                "canonical_url": "https://docs.example.com/releases",
                "content_hash": "a".repeat(64),
                "embedding": {
                    "model": "test",
                    "model_version": "v1",
                    "dimensions": 2,
                    "normalization": "unit_length",
                    "values": [0.6, 0.8]
                }
            }),
            diagnostic: Value::Null,
        };
        assert!(envelope.validate(&job).is_ok());
    }
}
