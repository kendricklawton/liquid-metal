//! Health check endpoint.
//!
//! Serves a JSON health response with node_id, uptime, and service counts.

use crate::deprovision;

pub fn spawn(
    node_id: String,
    registry: deprovision::VmRegistry,
    liquid_registry: deprovision::LiquidRegistry,
    listener: tokio::net::TcpListener,
) {
    let started = std::time::Instant::now();
    tokio::spawn(async move {
        use http_body_util::Full;
        use hyper::body::Bytes;
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use hyper::{Request, Response, StatusCode};

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    tracing::debug!(error = %e, "health accept error");
                    continue;
                }
            };
            let node_id = node_id.clone();
            let registry = registry.clone();
            let liquid = liquid_registry.clone();

            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let handler = service_fn(move |_req: Request<hyper::body::Incoming>| {
                    let node_id = node_id.clone();
                    let registry = registry.clone();
                    let liquid = liquid.clone();
                    async move {
                        let vm_count = registry.lock().await.len();
                        let wasm_count = liquid.lock().await.len();
                        let uptime = started.elapsed().as_secs();
                        let body = serde_json::json!({
                            "status": "ok",
                            "node_id": node_id,
                            "uptime_secs": uptime,
                            "metal_vms": vm_count,
                            "liquid_services": wasm_count,
                        });
                        Ok::<_, hyper::Error>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", "application/json")
                                .body(Full::new(Bytes::from(body.to_string())))
                                .unwrap(),
                        )
                    }
                });
                // Timeout prevents slow/stuck clients from holding a task indefinitely.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    http1::Builder::new().serve_connection(io, handler),
                ).await;
            });
        }
    });
}
