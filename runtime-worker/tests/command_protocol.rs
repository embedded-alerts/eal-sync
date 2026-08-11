#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
};

use eal_crawl_runtime::{
    runner::run_crawl_command,
    types::{CrawlCommandConfig, CrawlJob},
};
use uuid::Uuid;

const FIXTURE: &str = r#"#!/usr/bin/env python3
import hashlib
import json
import sys
import time

request = json.load(sys.stdin)
if request["start_url"].endswith("/timeout"):
    time.sleep(3)
content_hash = hashlib.sha256(request["start_url"].encode("utf-8")).hexdigest()
json.dump({
    "protocol_version": "eal-crawl-result/v1",
    "page_ingest": {
        "source_id": request["source_id"],
        "canonical_url": request["start_url"],
        "content_hash": content_hash,
        "embedding": {
            "model": "fixture-model",
            "model_version": "v1",
            "dimensions": 2,
            "normalization": "unit_length",
            "values": [0.6, 0.8]
        }
    },
    "diagnostic": {
        "fixture": True,
        "attempt_id": request["attempt_id"]
    }
}, sys.stdout)
"#;

fn fixture_path() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "eal-crawl-protocol-fixture-{}-{}.py",
        std::process::id(),
        Uuid::new_v4()
    ));
    fs::write(&path, FIXTURE).expect("write protocol fixture");
    let mut permissions = fs::metadata(&path).expect("fixture metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).expect("make protocol fixture executable");
    path
}

fn job(start_url: &str) -> CrawlJob {
    CrawlJob {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        source_id: Uuid::new_v4(),
        start_url: start_url.to_owned(),
        interval_seconds: 3600,
        attempt_count: 0,
        max_attempts: 12,
        lease_token: Uuid::new_v4(),
        attempt_id: Uuid::new_v4(),
    }
}

#[tokio::test]
async fn executes_versioned_protocol_and_validates_page_ingest() {
    let executable = fixture_path();
    let config = CrawlCommandConfig {
        executable: executable.clone(),
        timeout_seconds: 5,
        stdout_limit_bytes: 64 * 1024,
        stderr_limit_bytes: 4 * 1024,
    };
    let job = job("https://docs.example.com/releases");
    let output = run_crawl_command(&config, &job)
        .await
        .expect("run fixture crawler");
    assert_eq!(
        output.page_ingest.get("source_id").and_then(serde_json::Value::as_str),
        Some(job.source_id.to_string().as_str())
    );
    assert_eq!(
        output.diagnostic.get("fixture").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    fs::remove_file(executable).expect("remove protocol fixture");
}

#[tokio::test]
async fn kills_crawler_after_timeout() {
    let executable = fixture_path();
    let config = CrawlCommandConfig {
        executable: executable.clone(),
        timeout_seconds: 1,
        stdout_limit_bytes: 64 * 1024,
        stderr_limit_bytes: 4 * 1024,
    };
    let error = run_crawl_command(&config, &job("https://docs.example.com/timeout"))
        .await
        .expect_err("fixture must time out");
    assert_eq!(error.code(), "crawl_command_timeout");
    fs::remove_file(executable).expect("remove protocol fixture");
}

#[tokio::test]
async fn rejects_oversized_crawler_output() {
    let executable = fixture_path();
    let config = CrawlCommandConfig {
        executable: executable.clone(),
        timeout_seconds: 5,
        stdout_limit_bytes: 32,
        stderr_limit_bytes: 4 * 1024,
    };
    let error = run_crawl_command(&config, &job("https://docs.example.com/releases"))
        .await
        .expect_err("fixture output must exceed the limit");
    assert_eq!(error.code(), "crawl_command_stdout_limit");
    fs::remove_file(executable).expect("remove protocol fixture");
}
