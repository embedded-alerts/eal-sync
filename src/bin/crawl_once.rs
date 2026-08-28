//! Operator-only one-shot fetch for certifying a registered source.
//!
//! This binary intentionally accepts configuration only from process environment.
//! It is not mounted as an unauthenticated URL-fetch endpoint.

use eal_sync::ingestion::{
    ConditionalState, EmbeddingInputKind, FetchOutcome, FetchPolicy, FetchRequest, FetchScope,
    HttpFetcher,
};
use serde::Serialize;
use std::{env, error::Error};

#[derive(Debug, Serialize)]
struct CrawlSummary {
    outcome: &'static str,
    requested_url: String,
    canonical_url: String,
    final_url: String,
    status: u16,
    etag: Option<String>,
    last_modified: Option<String>,
    content_type: Option<String>,
    content_bytes: Option<usize>,
    content_sha256: Option<String>,
    content_preview: Option<String>,
    semantic: Option<SemanticSummary>,
}

#[derive(Debug, Serialize)]
struct SemanticSummary {
    title: Option<String>,
    keywords: Vec<String>,
    entities: Vec<String>,
    embedding_input_count: usize,
    embedding_kinds: Vec<&'static str>,
    embedding_preview: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let url = required_env("EAL_CRAWL_URL")?;
    let mut scope = FetchScope::for_url(&url)?;
    scope.include_subdomains = env_bool("EAL_CRAWL_INCLUDE_SUBDOMAINS")?;
    for host in env::var("EAL_CRAWL_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        scope.allow_host(host)?;
    }
    if let Some(prefixes) = nonempty_env("EAL_CRAWL_ALLOWED_PATH_PREFIXES") {
        scope.set_allowed_path_prefixes(
            prefixes
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        )?;
    }

    let request = FetchRequest {
        url,
        scope,
        conditional: ConditionalState {
            etag: nonempty_env("EAL_CRAWL_ETAG"),
            last_modified: nonempty_env("EAL_CRAWL_LAST_MODIFIED"),
        },
    };
    let fetcher = HttpFetcher::new(FetchPolicy::default())?;
    let summary = match fetcher.fetch(&request).await? {
        FetchOutcome::NotModified { metadata } => CrawlSummary {
            outcome: "not_modified",
            requested_url: metadata.requested_url,
            canonical_url: metadata.canonical_url,
            final_url: metadata.final_url,
            status: metadata.status,
            etag: metadata.etag,
            last_modified: metadata.last_modified,
            content_type: None,
            content_bytes: None,
            content_sha256: None,
            content_preview: None,
            semantic: None,
        },
        FetchOutcome::Changed { document } => {
            let semantic = SemanticSummary {
                title: document.title.clone(),
                keywords: document.keywords.clone(),
                entities: document.entities.clone(),
                embedding_input_count: document.embedding_inputs.len(),
                embedding_kinds: document
                    .embedding_inputs
                    .iter()
                    .map(|input| embedding_kind_name(input.kind))
                    .collect(),
                embedding_preview: preview(&document.embedding_text, 1_000),
            };
            CrawlSummary {
                outcome: "changed",
                requested_url: document.metadata.requested_url,
                canonical_url: document.metadata.canonical_url,
                final_url: document.metadata.final_url,
                status: document.metadata.status,
                etag: document.metadata.etag,
                last_modified: document.metadata.last_modified,
                content_type: Some(document.content_type),
                content_bytes: Some(document.content_bytes),
                content_sha256: Some(document.content_sha256),
                content_preview: Some(preview(&document.content_text, 500)),
                semantic: Some(semantic),
            }
        }
    };

    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn embedding_kind_name(kind: EmbeddingInputKind) -> &'static str {
    match kind {
        EmbeddingInputKind::Title => "title",
        EmbeddingInputKind::Heading => "heading",
        EmbeddingInputKind::Summary => "summary",
        EmbeddingInputKind::Sentence => "sentence",
        EmbeddingInputKind::Entity => "entity",
        EmbeddingInputKind::Keyword => "keyword",
        EmbeddingInputKind::UrlSignal => "url_signal",
        EmbeddingInputKind::Document => "document",
    }
}

fn required_env(name: &str) -> Result<String, Box<dyn Error>> {
    nonempty_env(name).ok_or_else(|| format!("{name} is required").into())
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn env_bool(name: &str) -> Result<bool, Box<dyn Error>> {
    let Some(value) = nonempty_env(name) else {
        return Ok(false);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{name} must be true or false").into()),
    }
}

fn preview(value: &str, max_characters: usize) -> String {
    let mut characters = value.chars();
    let mut preview = characters.by_ref().take(max_characters).collect::<String>();
    if characters.next().is_some() {
        preview.push('…');
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_unicode_safe() {
        assert_eq!(preview("alert 🔔 page", 7), "alert 🔔…");
    }

    #[test]
    fn embedding_kind_names_match_wire_values() {
        assert_eq!(
            embedding_kind_name(EmbeddingInputKind::UrlSignal),
            "url_signal"
        );
        assert_eq!(
            embedding_kind_name(EmbeddingInputKind::Sentence),
            "sentence"
        );
    }
}
