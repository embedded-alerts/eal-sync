use std::{fmt, net::IpAddr, time::Duration};

use futures_util::StreamExt;
use reqwest::{
    Client, StatusCode, Url,
    header::HeaderValue,
    redirect::Policy,
};
use serde_json::Value;
use uuid::Uuid;

const INGEST_TOKEN_HEADER: &str = "x-eal-ingest-token";
const MIN_INGEST_TOKEN_BYTES: usize = 32;
const MAX_INGEST_TOKEN_BYTES: usize = 512;

#[derive(Clone)]
pub struct IngestApiClient {
    client: Client,
    base_url: Url,
    ingest_token: HeaderValue,
    response_limit_bytes: usize,
}

impl IngestApiClient {
    pub fn new(
        base_url: &str,
        allow_loopback_http: bool,
        timeout_seconds: u64,
        response_limit_bytes: usize,
        ingest_token: &str,
    ) -> Result<Self, IngestApiError> {
        if !(1..=120).contains(&timeout_seconds) {
            return Err(IngestApiError::configuration(
                "API timeout must be between 1 and 120 seconds",
            ));
        }
        if !(64 * 1024..=16 * 1024 * 1024).contains(&response_limit_bytes) {
            return Err(IngestApiError::configuration(
                "API response limit must be between 65536 and 16777216 bytes",
            ));
        }
        let token_bytes = ingest_token.as_bytes();
        if ingest_token.trim() != ingest_token
            || !(MIN_INGEST_TOKEN_BYTES..=MAX_INGEST_TOKEN_BYTES).contains(&token_bytes.len())
        {
            return Err(IngestApiError::configuration(
                "ingest token must contain 32 to 512 bytes without leading or trailing whitespace",
            ));
        }
        let mut ingest_token = HeaderValue::from_str(ingest_token).map_err(|_| {
            IngestApiError::configuration("ingest token contains invalid HTTP header bytes")
        })?;
        ingest_token.set_sensitive(true);

        let mut base_url = Url::parse(base_url.trim()).map_err(|error| {
            IngestApiError::configuration(format!("invalid API base URL: {error}"))
        })?;
        if !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(IngestApiError::configuration(
                "API base URL must not contain credentials, query parameters, or fragments",
            ));
        }
        let host = base_url.host_str().ok_or_else(|| {
            IngestApiError::configuration("API base URL must contain a host")
        })?;
        match base_url.scheme() {
            "https" => {}
            "http" if allow_loopback_http && is_loopback_host(host) => {}
            "http" => {
                return Err(IngestApiError::configuration(
                    "plain HTTP is allowed only for an explicitly enabled loopback API",
                ));
            }
            _ => {
                return Err(IngestApiError::configuration(
                    "API base URL must use HTTPS",
                ));
            }
        }
        if base_url.path() != "/" && !base_url.path().is_empty() {
            return Err(IngestApiError::configuration(
                "API base URL must not contain a path prefix",
            ));
        }
        base_url.set_path("/");
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(timeout_seconds.min(10)))
            .timeout(Duration::from_secs(timeout_seconds))
            .user_agent("embedded-alerts-crawl-runtime/0.1")
            .build()
            .map_err(|error| IngestApiError::configuration(error.to_string()))?;
        Ok(Self {
            client,
            base_url,
            ingest_token,
            response_limit_bytes,
        })
    }

    pub async fn ingest_page(
        &self,
        tenant_id: Uuid,
        source_id: Uuid,
        payload: &Value,
    ) -> Result<Value, IngestApiError> {
        let url = self
            .base_url
            .join(&format!("v1/sources/{source_id}/pages"))
            .map_err(|error| IngestApiError::configuration(error.to_string()))?;
        let response = self
            .client
            .post(url)
            .header("x-eal-tenant-id", tenant_id.to_string())
            .header(INGEST_TOKEN_HEADER, self.ingest_token.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .json(payload)
            .send()
            .await
            .map_err(|error| IngestApiError::transport(error.to_string()))?;
        let status = response.status();
        if status.is_redirection() {
            return Err(IngestApiError::response(
                status,
                "API redirects are blocked",
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.response_limit_bytes as u64)
        {
            return Err(IngestApiError::response(
                status,
                "API response exceeded the byte limit",
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| IngestApiError::transport(error.to_string()))?;
            if bytes.len().saturating_add(chunk.len()) > self.response_limit_bytes {
                return Err(IngestApiError::response(
                    status,
                    "API response exceeded the byte limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice::<Value>(&bytes).map_err(|error| {
                IngestApiError::response(status, format!("API returned invalid JSON: {error}"))
            })?
        };
        if !status.is_success() {
            return Err(IngestApiError::response(
                status,
                safe_error_message(&value),
            ));
        }
        Ok(value)
    }
}

fn safe_error_message(value: &Value) -> String {
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .map(sanitize)
        .unwrap_or_else(|| "api_error".to_owned());
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .map(sanitize)
        .unwrap_or_else(|| "the API rejected the page revision".to_owned());
    format!("{code}: {message}")
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(300)
        .collect()
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[derive(Debug)]
pub struct IngestApiError {
    code: &'static str,
    status: Option<StatusCode>,
    message: String,
}

impl IngestApiError {
    fn configuration(message: impl Into<String>) -> Self {
        Self {
            code: "api_configuration",
            status: None,
            message: message.into(),
        }
    }

    fn transport(message: impl Into<String>) -> Self {
        Self {
            code: "api_transport",
            status: None,
            message: message.into(),
        }
    }

    fn response(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            code: "api_response",
            status: Some(status),
            message: message.into(),
        }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub fn diagnostic(&self) -> Value {
        serde_json::json!({
            "error_code": self.code,
            "status": self.status.map(|status| status.as_u16()),
            "message": self.message.chars().take(500).collect::<String>(),
        })
    }
}

impl fmt::Display for IngestApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(status) = self.status {
            write!(formatter, "API returned {status}: {}", self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for IngestApiError {}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn remote_plain_http_api_is_rejected() {
        assert!(
            IngestApiClient::new("http://example.com", true, 20, 2_097_152, TOKEN).is_err()
        );
    }

    #[test]
    fn loopback_plain_http_requires_explicit_opt_in() {
        assert!(
            IngestApiClient::new("http://127.0.0.1:8080", false, 20, 2_097_152, TOKEN)
                .is_err()
        );
        assert!(
            IngestApiClient::new("http://127.0.0.1:8080", true, 20, 2_097_152, TOKEN)
                .is_ok()
        );
    }

    #[test]
    fn ingest_token_is_required_and_bounded() {
        assert!(
            IngestApiClient::new("https://api.example.com", false, 20, 2_097_152, "short")
                .is_err()
        );
        assert!(
            IngestApiClient::new(
                "https://api.example.com",
                false,
                20,
                2_097_152,
                &format!(" {TOKEN}")
            )
            .is_err()
        );
    }
}
