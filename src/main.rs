use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{env, net::SocketAddr};
use syncer_rs::{merge_json, ArrayMergeStrategy, MergeOptions};
use tower_http::trace::TraceLayer;
use tracing::info;

#[derive(Clone)]
struct AppState { options: MergeOptions }

#[derive(Debug, Deserialize)]
struct ReconcileRequest { base: Value, incoming: Value }

#[derive(Debug, Serialize)]
struct ReconcileResponse { merged: Value, engine: &'static str, contract: &'static str }

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=info".into())
    ).init();
    let options = MergeOptions {
        array_strategy: ArrayMergeStrategy::MergeByKey,
        resolve_by_timestamp: true,
        lww_keys: Some("updated_at,synced_at".into()),
        array_match_keys: Some("id".into()),
        ..MergeOptions::default()
    };
    let app = Router::new()
        .route("/healthz", axum::routing::get(|| async { Json(serde_json::json!({"status":"ok","service":"eal-sync"})) }))
        .route("/readyz", axum::routing::get(|| async { StatusCode::NO_CONTENT }))
        .route("/api/v1/reconcile", post(reconcile))
        .layer(TraceLayer::new_for_http())
        .with_state(AppState { options });
    let addr: SocketAddr = env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into()).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "Embedded Alerts sync gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn reconcile(
    State(state): State<AppState>,
    Json(request): Json<ReconcileRequest>,
) -> Result<Json<ReconcileResponse>, (StatusCode, Json<Value>)> {
    let base = serde_json::to_string(&request.base).map_err(internal)?;
    let incoming = serde_json::to_string(&request.incoming).map_err(internal)?;
    let merged = merge_json(&base, &incoming, &state.options)
        .map_err(|error| (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({"error":"merge_failed","message":error.to_string()}))))?;
    let merged: Value = serde_json::from_str(&merged).map_err(internal)?;
    Ok(Json(ReconcileResponse { merged, engine: "opto-sync/syncer.rs", contract: "eal_interfaces::AlertRule" }))
}

fn internal(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error":"internal","message":error.to_string()})))
}
