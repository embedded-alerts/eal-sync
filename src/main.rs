use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{env, net::SocketAddr};
use syncer_rs::{ArrayMergeStrategy, MergeOptions, merge_json};
use tower_http::trace::TraceLayer;
use tracing::info;

#[derive(Clone)]
struct AppState {
    options: MergeOptions,
}

#[derive(Debug, Deserialize)]
struct ReconcileRequest {
    base: Value,
    incoming: Value,
}

#[derive(Debug, Serialize)]
struct ReconcileResponse {
    merged: Value,
    engine: &'static str,
    contract: &'static str,
}

fn merge_options() -> MergeOptions {
    MergeOptions {
        array_strategy: ArrayMergeStrategy::MergeByKey,
        resolve_by_timestamp: true,
        lww_keys: Some("updated_at,synced_at".into()),
        array_match_keys: Some("id".into()),
        ..MergeOptions::default()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();

    let app = Router::new()
        .route(
            "/healthz",
            get(|| async { Json(serde_json::json!({"status":"ok","service":"eal-sync"})) }),
        )
        .route("/readyz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/v1/reconcile", post(reconcile))
        .route("/api/v1/reconcile", post(reconcile))
        .layer(TraceLayer::new_for_http())
        .with_state(AppState {
            options: merge_options(),
        });

    let addr: SocketAddr = env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8084".into())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "Embedded Alerts sync gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn reconcile(
    State(state): State<AppState>,
    Json(request): Json<ReconcileRequest>,
) -> Result<Json<ReconcileResponse>, (StatusCode, Json<Value>)> {
    let merged =
        reconcile_values(&request.base, &request.incoming, &state.options).map_err(|error| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "merge_failed",
                    "message": error.to_string()
                })),
            )
        })?;

    Ok(Json(ReconcileResponse {
        merged,
        engine: "opto-sync/syncer.rs",
        contract: "eal_interfaces::AlertRule",
    }))
}

fn reconcile_values(
    base: &Value,
    incoming: &Value,
    options: &MergeOptions,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let base = serde_json::to_string(base)?;
    let incoming = serde_json::to_string(incoming)?;
    let merged = merge_json(&base, &incoming, options)?;
    Ok(serde_json::from_str(&merged)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_alert_rules_by_id_and_accepts_newer_state() {
        let base = serde_json::json!([
            {"id":"rule-1","updated_at":"2026-08-04T10:00:00Z","query_text":"old"}
        ]);
        let incoming = serde_json::json!([
            {"id":"rule-1","updated_at":"2026-08-04T11:00:00Z","query_text":"new"},
            {"id":"rule-2","updated_at":"2026-08-04T11:00:00Z","query_text":"second"}
        ]);
        let merged = reconcile_values(&base, &incoming, &merge_options()).unwrap();
        assert_eq!(merged[0]["query_text"], "new");
        assert_eq!(merged[1]["id"], "rule-2");
    }

    #[test]
    fn rejects_older_last_writer_state() {
        let base = serde_json::json!([
            {"id":"rule-1","updated_at":"2026-08-04T11:00:00Z","query_text":"current"}
        ]);
        let incoming = serde_json::json!([
            {"id":"rule-1","updated_at":"2026-08-04T10:00:00Z","query_text":"stale"}
        ]);
        let merged = reconcile_values(&base, &incoming, &merge_options()).unwrap();
        assert_eq!(merged[0]["query_text"], "current");
    }
}
