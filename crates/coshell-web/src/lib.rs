use axum::{extract::State, routing::get, Json, Router};
use coshell_core::RuntimeService;
use coshell_types::HealthReport;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[derive(Clone, Default)]
pub struct WebState {
    runtime: RuntimeService,
}

async fn health(State(state): State<WebState>) -> Json<HealthReport> {
    Json(state.runtime.health())
}

pub fn router() -> Router {
    Router::new()
        .route("/api/health", get(health))
        .with_state(WebState::default())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}
