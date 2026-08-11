use crate::ingestion::IngestionError;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName};

pub(super) fn header_string(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

pub(super) fn content_length(headers: &HeaderMap) -> Result<Option<u64>, IngestionError> {
    headers
        .get(CONTENT_LENGTH)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| {
                    IngestionError::new(
                        "invalid_content_length",
                        "Content-Length is not valid ASCII",
                    )
                })?
                .parse::<u64>()
                .map_err(|_| {
                    IngestionError::new(
                        "invalid_content_length",
                        "Content-Length is not an unsigned integer",
                    )
                })
        })
        .transpose()
}

pub(super) fn content_type(
    headers: &HeaderMap,
    required: bool,
) -> Result<String, IngestionError> {
    match headers.get(CONTENT_TYPE) {
        Some(value) => value
            .to_str()
            .map(str::to_ascii_lowercase)
            .map_err(|_| {
                IngestionError::new(
                    "invalid_content_type",
                    "Content-Type is not valid ASCII/UTF-8",
                )
            }),
        None if required => Err(IngestionError::new(
            "missing_content_type",
            "response omitted Content-Type",
        )),
        None => Ok("text/plain".into()),
    }
}

pub(super) fn is_allowed_content_type(value: &str) -> bool {
    let base = value.split(';').next().unwrap_or(value).trim();
    matches!(
        base,
        "text/html"
            | "application/xhtml+xml"
            | "text/plain"
            | "application/json"
            | "application/feed+json"
            | "application/xml"
            | "text/xml"
            | "application/rss+xml"
            | "application/atom+xml"
    ) || base.ends_with("+json")
        || base.ends_with("+xml")
}

pub(super) async fn read_bounded_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, IngestionError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            IngestionError::new(
                "body_read_failed",
                format!("could not read response body: {error}"),
            )
        })?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(IngestionError::new(
                "response_too_large",
                format!("decompressed response exceeds {max_bytes} bytes"),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
