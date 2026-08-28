use super::{
    FetchPolicy, FetchRequest,
    response::{
        content_length, content_type, header_string, is_allowed_content_type, read_bounded_body,
    },
    safety::{canonical_identity, prepare_target_url, resolve_public_addresses},
};
use crate::ingestion::{EmbeddingInput, IngestionError, extract_semantic_document};
use eal_semantic::sha256_hex;
use reqwest::{
    Client, StatusCode,
    header::{
        ACCEPT, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, LOCATION, RETRY_AFTER,
    },
    redirect::Policy,
};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use url::Url;

const DEFAULT_ACCEPT: &str = "text/html,application/xhtml+xml,application/json,application/feed+json,application/rss+xml,application/atom+xml,application/xml,text/xml,text/plain;q=0.8";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FetchMetadata {
    pub requested_url: String,
    pub canonical_url: String,
    pub final_url: String,
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FetchedDocument {
    #[serde(flatten)]
    pub metadata: FetchMetadata,
    pub content_type: String,
    pub content_bytes: usize,
    pub content_sha256: String,
    pub title: Option<String>,
    pub content_text: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub embedding_text: String,
    pub embedding_inputs: Vec<EmbeddingInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FetchOutcome {
    NotModified { metadata: FetchMetadata },
    Changed { document: FetchedDocument },
}

#[derive(Clone)]
pub struct HttpFetcher {
    policy: FetchPolicy,
    host_limiters: Arc<Mutex<BTreeMap<String, Arc<Semaphore>>>>,
}

impl HttpFetcher {
    pub fn new(policy: FetchPolicy) -> Result<Self, IngestionError> {
        policy.validate()?;
        Ok(Self {
            policy,
            host_limiters: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub async fn fetch(&self, request: &FetchRequest) -> Result<FetchOutcome, IngestionError> {
        let requested = prepare_target_url(&request.url, &request.scope)?;
        let requested_host = requested
            .host_str()
            .ok_or_else(|| IngestionError::new("invalid_url", "URL host is required"))?
            .to_ascii_lowercase();
        let mut current = requested.clone();

        for redirect_count in 0..=self.policy.max_redirects {
            let current_host = current
                .host_str()
                .ok_or_else(|| IngestionError::new("invalid_url", "URL host is required"))?
                .to_ascii_lowercase();
            let _permit = self.acquire_host_permit(&current_host).await?;
            let client = self.pinned_client(&current).await?;
            let mut builder = client.get(current.clone()).header(ACCEPT, DEFAULT_ACCEPT);
            if current_host == requested_host {
                if let Some(etag) = request.conditional.etag.as_deref() {
                    builder = builder.header(IF_NONE_MATCH, etag);
                }
                if let Some(last_modified) = request.conditional.last_modified.as_deref() {
                    builder = builder.header(IF_MODIFIED_SINCE, last_modified);
                }
            }

            let response = builder.send().await.map_err(|error| {
                IngestionError::new("request_failed", format!("HTTP request failed: {error}"))
            })?;
            let status = response.status();
            let headers = response.headers().clone();

            if status == StatusCode::NOT_MODIFIED {
                let canonical_url = canonical_identity(&current)?;
                return Ok(FetchOutcome::NotModified {
                    metadata: FetchMetadata {
                        requested_url: requested.as_str().into(),
                        canonical_url,
                        final_url: current.as_str().into(),
                        status: status.as_u16(),
                        etag: header_string(&headers, &ETAG)
                            .or_else(|| request.conditional.etag.clone()),
                        last_modified: header_string(&headers, &LAST_MODIFIED)
                            .or_else(|| request.conditional.last_modified.clone()),
                    },
                });
            }

            if status.is_redirection() {
                if redirect_count == self.policy.max_redirects {
                    return Err(IngestionError::new(
                        "redirect_limit",
                        format!("redirect limit of {} exceeded", self.policy.max_redirects),
                    ));
                }
                let location = headers
                    .get(LOCATION)
                    .ok_or_else(|| {
                        IngestionError::new(
                            "invalid_redirect",
                            "redirect response omitted the Location header",
                        )
                    })?
                    .to_str()
                    .map_err(|_| {
                        IngestionError::new(
                            "invalid_redirect",
                            "redirect Location is not valid ASCII/UTF-8",
                        )
                    })?;
                let redirected = current.join(location).map_err(|error| {
                    IngestionError::new(
                        "invalid_redirect",
                        format!("could not resolve redirect target: {error}"),
                    )
                })?;
                current = prepare_target_url(redirected.as_str(), &request.scope)?;
                continue;
            }

            if !status.is_success() {
                let retry_after = header_string(&headers, &RETRY_AFTER)
                    .map(|value| format!("; retry-after={value}"))
                    .unwrap_or_default();
                return Err(IngestionError::new(
                    if matches!(status.as_u16(), 429 | 503) {
                        "remote_throttled"
                    } else {
                        "remote_status"
                    },
                    format!("remote returned HTTP {}{retry_after}", status.as_u16()),
                ));
            }

            if let Some(length) = content_length(&headers)? {
                if length > self.policy.max_response_bytes as u64 {
                    return Err(IngestionError::new(
                        "response_too_large",
                        format!(
                            "Content-Length {length} exceeds {} bytes",
                            self.policy.max_response_bytes
                        ),
                    ));
                }
            }

            let content_type = content_type(&headers, self.policy.require_content_type)?;
            if !is_allowed_content_type(&content_type) {
                return Err(IngestionError::new(
                    "unsupported_content_type",
                    format!("content type {content_type:?} is not ingestible"),
                ));
            }
            let etag = header_string(&headers, &ETAG);
            let last_modified = header_string(&headers, &LAST_MODIFIED);
            let body = read_bounded_body(response, self.policy.max_response_bytes).await?;
            let canonical_url = canonical_identity(&current)?;
            let semantic = extract_semantic_document(&content_type, &body, &canonical_url)?;
            let content_sha256 = sha256_hex(semantic.content_text.as_bytes());
            let embedding_text = semantic.combined_embedding_text();
            let metadata = FetchMetadata {
                requested_url: requested.as_str().into(),
                canonical_url,
                final_url: current.as_str().into(),
                status: status.as_u16(),
                etag,
                last_modified,
            };
            return Ok(FetchOutcome::Changed {
                document: FetchedDocument {
                    metadata,
                    content_type,
                    content_bytes: body.len(),
                    content_sha256,
                    title: semantic.title,
                    content_text: semantic.content_text,
                    keywords: semantic.keywords,
                    entities: semantic.entities,
                    embedding_text,
                    embedding_inputs: semantic.embedding_inputs,
                },
            });
        }

        Err(IngestionError::new(
            "redirect_limit",
            "redirect processing terminated unexpectedly",
        ))
    }

    async fn acquire_host_permit(
        &self,
        host: &str,
    ) -> Result<OwnedSemaphorePermit, IngestionError> {
        let semaphore = {
            let mut limiters = self.host_limiters.lock().await;
            limiters
                .entry(host.to_owned())
                .or_insert_with(|| Arc::new(Semaphore::new(self.policy.max_concurrency_per_host)))
                .clone()
        };
        semaphore.acquire_owned().await.map_err(|_| {
            IngestionError::new("worker_shutdown", "host concurrency limiter was closed")
        })
    }

    async fn pinned_client(&self, url: &Url) -> Result<Client, IngestionError> {
        let host = url
            .host_str()
            .ok_or_else(|| IngestionError::new("invalid_url", "URL host is required"))?;
        let port = url.port_or_known_default().ok_or_else(|| {
            IngestionError::new("invalid_url", "URL does not have a usable network port")
        })?;
        let addresses = resolve_public_addresses(host, port).await?;
        let selected = addresses
            .iter()
            .find(|address| address.is_ipv4())
            .or_else(|| addresses.first())
            .copied()
            .ok_or_else(|| {
                IngestionError::new("dns_no_public_address", "host has no public address")
            })?;
        Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(self.policy.request_timeout)
            .connect_timeout(self.policy.connect_timeout)
            .user_agent(&self.policy.user_agent)
            .resolve(host, selected)
            .build()
            .map_err(|error| {
                IngestionError::new(
                    "client_build_failed",
                    format!("could not build pinned HTTP client: {error}"),
                )
            })
    }
}
