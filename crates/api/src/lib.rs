pub mod grpc;
pub mod migrations;
pub mod nats;
pub mod routes;
pub mod storage;

pub mod proto {
    pub mod liquidmetal {
        pub mod v1 {
            tonic::include_proto!("liquidmetal.v1");
        }
    }
}

use axum::{Router, middleware, routing::get};
use grpc::service::ServiceServiceImpl;
use grpc::user::UserServiceImpl;
use grpc::workspace::WorkspaceServiceImpl;
use proto::liquidmetal::v1::service_service_server::ServiceServiceServer;
use proto::liquidmetal::v1::user_service_server::UserServiceServer;
use proto::liquidmetal::v1::workspace_service_server::WorkspaceServiceServer;
use std::sync::Arc;
use tonic::service::Routes as TonicRoutes;
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub db:              deadpool_postgres::Pool,
    pub nats:            async_nats::jetstream::Context,
    pub nats_client:     async_nats::Client,
    pub s3:              aws_sdk_s3::Client,
    pub bucket:          String,
    pub internal_secret: String,
}

/// Assemble the full Axum + tonic application router.
pub fn build_router(state: Arc<AppState>) -> Router {
    // Public — no auth
    let public = Router::new()
        .route("/healthz", get(routes::health))
        .with_state(state.clone());

    // Internal — X-Internal-Secret; called by Go web BFF only
    let internal = Router::new()
        .route("/auth/provision", axum::routing::post(routes::provision_user))
        .with_state(state.clone());

    // CLI — X-Api-Key; called by flux CLI
    let cli = Router::new()
        .nest("/services", routes::services_router())
        .route("/upload-url", get(routes::upload_url))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            routes::auth_middleware,
        ))
        .with_state(state.clone());

    let rest = public
        .merge(internal)
        .merge(cli)
        .layer(TraceLayer::new_for_http());

    let grpc = TonicRoutes::new(ServiceServiceServer::new(ServiceServiceImpl {
        state: state.clone(),
    }))
    .add_service(UserServiceServer::new(UserServiceImpl {
        state: state.clone(),
    }))
    .add_service(WorkspaceServiceServer::new(WorkspaceServiceImpl {
        state: state.clone(),
    }))
    .into_axum_router();

    rest.merge(grpc)
}
