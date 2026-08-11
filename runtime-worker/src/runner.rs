use std::{fmt, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

use crate::types::{
    CrawlCommandConfig, CrawlCommandEnvelope, CrawlCommandOutput, CrawlCommandRequest, CrawlJob,
};

pub async fn run_crawl_command(
    config: &CrawlCommandConfig,
    job: &CrawlJob,
) -> Result<CrawlCommandOutput, CrawlRunError> {
    if !config.executable.is_absolute() {
        return Err(CrawlRunError::new(
            "crawl_command_invalid",
            "crawl command path must be absolute",
        ));
    }
    let request = serde_json::to_vec(&CrawlCommandRequest::from(job)).map_err(|error| {
        CrawlRunError::new("crawl_request_encode", format!("encode crawl request: {error}"))
    })?;
    let mut child = Command::new(&config.executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            CrawlRunError::new("crawl_command_spawn", format!("spawn crawl command: {error}"))
        })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        CrawlRunError::new("crawl_command_stdin", "crawl command stdin was unavailable")
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        CrawlRunError::new("crawl_command_stdout", "crawl command stdout was unavailable")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        CrawlRunError::new("crawl_command_stderr", "crawl command stderr was unavailable")
    })?;

    stdin.write_all(&request).await.map_err(|error| {
        CrawlRunError::new(
            "crawl_command_stdin",
            format!("write crawl request: {error}"),
        )
    })?;
    stdin.write_all(b"\n").await.map_err(|error| {
        CrawlRunError::new(
            "crawl_command_stdin",
            format!("terminate crawl request: {error}"),
        )
    })?;
    stdin.shutdown().await.map_err(|error| {
        CrawlRunError::new(
            "crawl_command_stdin",
            format!("close crawl command stdin: {error}"),
        )
    })?;

    let run = async {
        let (stdout, stderr, status) = tokio::join!(
            read_limited(stdout, config.stdout_limit_bytes),
            read_limited(stderr, config.stderr_limit_bytes),
            child.wait(),
        );
        (stdout, stderr, status)
    };
    let (stdout, stderr, status) = match timeout(Duration::from_secs(config.timeout_seconds), run).await
    {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            return Err(CrawlRunError::new(
                "crawl_command_timeout",
                "crawl command exceeded its execution deadline",
            ));
        }
    };
    let stdout = stdout.map_err(|error| {
        CrawlRunError::new(
            "crawl_command_stdout",
            format!("read crawl command stdout: {error}"),
        )
    })?;
    let stderr = stderr.map_err(|error| {
        CrawlRunError::new(
            "crawl_command_stderr",
            format!("read crawl command stderr: {error}"),
        )
    })?;
    let status = status.map_err(|error| {
        CrawlRunError::new(
            "crawl_command_wait",
            format!("wait for crawl command: {error}"),
        )
    })?;
    if stdout.exceeded {
        return Err(CrawlRunError::new(
            "crawl_command_stdout_limit",
            "crawl command stdout exceeded its byte limit",
        ));
    }
    if stderr.exceeded {
        return Err(CrawlRunError::new(
            "crawl_command_stderr_limit",
            "crawl command stderr exceeded its byte limit",
        ));
    }
    if !status.success() {
        let diagnostic = sanitize_diagnostic(&stderr.bytes);
        return Err(CrawlRunError::new(
            "crawl_command_exit",
            if diagnostic.is_empty() {
                format!("crawl command exited with {status}")
            } else {
                format!("crawl command exited with {status}: {diagnostic}")
            },
        ));
    }

    let envelope = serde_json::from_slice::<CrawlCommandEnvelope>(&stdout.bytes).map_err(|error| {
        CrawlRunError::new(
            "crawl_command_protocol",
            format!("decode crawl command result: {error}"),
        )
    })?;
    envelope.validate(job).map_err(|error| {
        CrawlRunError::new(
            "crawl_command_protocol",
            format!("validate crawl command result: {error}"),
        )
    })
}

struct LimitedBytes {
    bytes: Vec<u8>,
    exceeded: bool,
}

async fn read_limited<R>(reader: R, limit: usize) -> std::io::Result<LimitedBytes>
where
    R: AsyncRead + Unpin,
{
    let mut reader = reader.take(limit.saturating_add(1) as u64);
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    reader.read_to_end(&mut bytes).await?;
    let exceeded = bytes.len() > limit;
    if exceeded {
        bytes.truncate(limit);
    }
    Ok(LimitedBytes { bytes, exceeded })
}

fn sanitize_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(500)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[derive(Debug)]
pub struct CrawlRunError {
    code: &'static str,
    message: String,
}

impl CrawlRunError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub fn diagnostic(&self) -> serde_json::Value {
        serde_json::json!({
            "error_code": self.code,
            "message": self.message.chars().take(500).collect::<String>(),
        })
    }
}

impl fmt::Display for CrawlRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CrawlRunError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_remove_control_characters_and_are_bounded() {
        let text = sanitize_diagnostic("hello\nsecret\u{0000}".as_bytes());
        assert_eq!(text, "hellosecret");
        assert!(sanitize_diagnostic(&vec![b'x'; 1000]).len() <= 500);
    }
}
