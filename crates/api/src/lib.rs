pub mod grpc;
pub mod migrations;
pub mod nats;
pub mod quota;
pub mod routes;
pub mod storage;

pub mod proto {
    pub mod liquidmetal {
        pub mod v1 {
            // This macro includes the code generated from your .proto files
            // during the build process (service, user, workspace, and project).
            tonic::include_proto!("liquidmetal.v1");
        }
    }
}

use axum::{
    Router, middleware,
    routing::{get, post},
};
use std::sync::Arc;
use tonic::service::Routes as TonicRoutes;
use tower_http::trace::TraceLayer;

// Hand-written gRPC implementations
use grpc::project::ProjectServiceImpl;
use grpc::service::ServiceServiceImpl;
use grpc::user::UserServiceImpl;
use grpc::workspace::WorkspaceServiceImpl;

// Generated gRPC server traits
use proto::liquidmetal::v1::project_service_server::ProjectServiceServer;
use proto::liquidmetal::v1::service_service_server::ServiceServiceServer;
use proto::liquidmetal::v1::user_service_server::UserServiceServer;
use proto::liquidmetal::v1::workspace_service_server::WorkspaceServiceServer;

pub struct AppState {
    pub db: deadpool_postgres::Pool,
    pub nats: async_nats::jetstream::Context,
    pub nats_client: async_nats::Client,
    pub s3: aws_sdk_s3::Client,
    pub bucket: String,
    pub internal_secret: String,
    pub zitadel_domain: String,
    pub zitadel_client_id: String,
    pub features: common::Features,
}

/// Assemble the full Axum + tonic application router.
pub fn build_router(state: Arc<AppState>) -> Router {
    // ─── PUBLIC ROUTES ────────────────────────────────────────────────────────
    // Routes that require zero authentication (health checks, etc.)
    let public = Router::new()
        .route("/healthz",        get(routes::health))
        .route("/auth/cli/config", get(routes::cli_config))
        .with_state(state.clone());

    // ─── INTERNAL ROUTES (BFF / Web UI only) ──────────────────────────────────
    // Routes used by your Go dashboard or internal services.
    // All protected by X-Internal-Secret — never exposed to end users.
    let internal = Router::new()
        .route("/auth/provision", post(routes::provision_user))
        .route("/admin/invites",  post(routes::create_invites))
        .with_state(state.clone());

    // ─── CLI AUTH ROUTES ───────────────────────────────────────────────────────
    // Called by the CLI during device flow login. No auth required —
    // the Zitadel device flow itself is the proof of identity.
    let cli_auth = Router::new()
        .route("/auth/cli/provision", post(routes::cli_provision))
        .with_state(state.clone());

    // ─── GRPC / CONNECT RPC ROUTES (CLI & Web UI) ─────────────────────────────
    // We combine all individual Tonic services into a single unified router.
    // ConnectRPC clients (like your Go CLI) will hit these via standard HTTP POST.
    let grpc_routes = TonicRoutes::new(ServiceServiceServer::new(ServiceServiceImpl {
        state: state.clone(),
    }))
    .add_service(UserServiceServer::new(UserServiceImpl {
        state: state.clone(),
    }))
    .add_service(WorkspaceServiceServer::new(WorkspaceServiceImpl {
        state: state.clone(),
    }))
    .add_service(ProjectServiceServer::new(ProjectServiceImpl {
        state: state.clone(),
    }))
    .into_axum_router();

    // ─── SECURITY LAYER ───────────────────────────────────────────────────────
    // Every gRPC request must provide a valid API Key/Bearer token.
    // This middleware intercepts the HTTP request before it reaches Tonic.
    let secure_grpc = grpc_routes.route_layer(middleware::from_fn_with_state(
        state.clone(),
        routes::auth_middleware,
    ));

    // ─── FINAL ASSEMBLY ───────────────────────────────────────────────────────
    public
        .merge(internal)
        .merge(cli_auth)
        .merge(secure_grpc)
        .layer(TraceLayer::new_for_http())
}
