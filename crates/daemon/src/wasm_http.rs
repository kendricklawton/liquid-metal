/// Per-request Wasm HTTP shim — WAGI (WebAssembly Gateway Interface).
///
/// Analogous to CGI but for Wasm: for every inbound HTTP request the daemon
/// spins up a fresh Wasm instance, feeds the request via WASI env vars + stdin,
/// and reads the response from stdout. The module is compiled once at startup
/// and shared (via Arc) across all request handlers for that service.
///
/// WAGI response format (written to stdout by the Wasm binary):
///
///   Content-Type: text/plain
///   Status: 200 OK          ← optional; defaults to 200
///
///   <response body>
///
/// Any Go binary compiled with `GOOS=wasip1 GOARCH=wasm` works out of the box:
/// read REQUEST_METHOD / PATH_INFO / QUERY_STRING from env, read body from
/// stdin, write the CGI-style response to stdout.
use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use std::sync::Arc;
use tokio::net::TcpListener;
use wasmtime::*;
use wasmtime_wasi::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::{I32Exit, WasiCtxBuilder};

/// Fuel budget per request — 1 billion units ≈ several seconds of CPU.
const WASM_FUEL: u64 = 1_000_000_000;

/// Maximum Wasm stack depth.
const WASM_STACK_BYTES: usize = 1024 * 1024;

/// Maximum captured stdout size (4 MiB).
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Pre-compiled Wasm service shared across all request-handling tasks.
struct WasmService {
    engine:   Arc<Engine>,
    module:   Arc<Module>,
    app_name: String,
}

/// Compile `wasm_path`, bind a local TCP listener, and start serving requests.
/// Returns the port number written to the `services` table as `upstream_addr`.
pub async fn serve(wasm_path: String, app_name: String) -> Result<u16> {
    // Compile once — expensive (~100 ms for a Go Wasm binary); amortised across
    // every request for the lifetime of this service.
    let mut cfg = Config::new();
    cfg.consume_fuel(true);
    cfg.max_wasm_stack(WASM_STACK_BYTES);
    let engine = Arc::new(Engine::new(&cfg).context("wasmtime engine")?);

    let wasm_bytes = tokio::fs::read(&wasm_path)
        .await
        .with_context(|| format!("reading {wasm_path}"))?;

    let module = Arc::new(
        Module::new(&engine, &wasm_bytes).context("compiling wasm module")?,
    );

    // Bind on an OS-assigned port so we never collide across services.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding wasm HTTP listener")?;
    let port = listener.local_addr()?.port();

    tracing::info!(app = app_name, port, "liquid wasm HTTP shim ready");

    let svc = Arc::new(WasmService { engine, module, app_name });

    tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(x)  => x,
                Err(e) => { tracing::error!(error = %e, "accept error"); continue; }
            };

            let svc = svc.clone();
            tokio::spawn(async move {
                let io      = hyper_util::rt::TokioIo::new(stream);
                let handler = service_fn(move |req| {
                    let svc = svc.clone();
                    async move { dispatch(svc, req).await }
                });
                if let Err(e) = http1::Builder::new().serve_connection(io, handler).await {
                    tracing::debug!(error = %e, ?peer, "http connection closed");
                }
            });
        }
    });

    Ok(port)
}

/// Handle a single HTTP request: extract context, run Wasm, return response.
async fn dispatch(
    svc: Arc<WasmService>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let method = req.method().to_string();
    let uri    = req.uri().clone();
    let path   = uri.path().to_string();
    let query  = uri.query().unwrap_or("").to_string();

    // Build WAGI/CGI env vars from the request.
    let mut env_vars: Vec<(String, String)> = vec![
        ("REQUEST_METHOD".into(), method),
        ("PATH_INFO".into(),      path),
        ("QUERY_STRING".into(),   query),
        ("SERVER_PROTOCOL".into(), "HTTP/1.1".into()),
    ];

    for (k, v) in req.headers() {
        let name = k.as_str();
        let val  = v.to_str().unwrap_or("").to_string();
        if name.eq_ignore_ascii_case("content-type") {
            env_vars.push(("CONTENT_TYPE".into(), val.clone()));
        }
        let env_key = format!("HTTP_{}", name.to_uppercase().replace('-', "_"));
        env_vars.push((env_key, val));
    }

    let body_bytes: Vec<u8> = match req.into_body().collect().await {
        Ok(b)  => b.to_bytes().to_vec(),
        Err(_) => vec![],
    };
    env_vars.push(("CONTENT_LENGTH".into(), body_bytes.len().to_string()));

    let engine   = svc.engine.clone();
    let module   = svc.module.clone();
    let app_name = svc.app_name.clone();

    let result = tokio::task::spawn_blocking(move || {
        invoke(&engine, &module, &app_name, env_vars, body_bytes)
    })
    .await;

    match result {
        Ok(Ok((status, headers, body))) => {
            let mut builder = Response::builder().status(status);
            for (k, v) in headers {
                builder = builder.header(k, v);
            }
            Ok(builder.body(Full::new(Bytes::from(body))).unwrap())
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, app = svc.app_name, "wasm execution error");
            Ok(error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
        Err(e) => {
            tracing::error!(error = ?e, "wasm task panicked");
            Ok(error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into()))
        }
    }
}

/// Synchronous Wasm invocation — runs in a blocking thread pool task.
fn invoke(
    engine:   &Engine,
    module:   &Module,
    app_name: &str,
    env_vars: Vec<(String, String)>,
    body:     Vec<u8>,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut linker: Linker<WasiP1Ctx> = Linker::new(engine);
    preview1::add_to_linker_sync(&mut linker, |ctx| ctx)
        .context("adding WASI preview1 to linker")?;

    // Capture stdout — shared handle so we can read bytes after execution.
    let stdout = MemoryOutputPipe::new(MAX_RESPONSE_BYTES);
    let stdin  = MemoryInputPipe::new(Bytes::from(body));

    let env_pairs: Vec<(&str, &str)> = env_vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let wasi = WasiCtxBuilder::new()
        .envs(&env_pairs)
        .stdin(stdin)
        .stdout(stdout.clone())
        .build_p1();

    let mut store = Store::new(engine, wasi);
    store.set_fuel(WASM_FUEL).context("setting wasm fuel")?;

    let instance = linker
        .instantiate(&mut store, module)
        .context("instantiating wasm module")?;

    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .context("wasm module must export _start")?;

    match start.call(&mut store, ()) {
        Ok(()) => {}
        Err(err) => {
            // WASI proc_exit(0) is a clean, normal termination for CGI handlers.
            if err.downcast_ref::<I32Exit>().map(|e| e.0 == 0).unwrap_or(false) {
                // clean exit — fall through
            } else {
                return Err(err.context("wasm execution failed"));
            }
        }
    }

    let output = stdout.contents().to_vec();
    tracing::debug!(app = app_name, response_bytes = output.len(), "wasm request done");

    parse_wagi_response(output)
}

/// Parse CGI/WAGI response written by the Wasm module to stdout.
fn parse_wagi_response(
    output: Vec<u8>,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let (header_bytes, body) = split_on_blank_line(&output);

    let header_str = std::str::from_utf8(header_bytes)
        .context("response headers are not valid UTF-8")?;

    let mut status: u16 = 200;
    let mut headers: Vec<(String, String)> = Vec::new();

    for line in header_str.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            if k.eq_ignore_ascii_case("status") {
                // "200 OK" or bare "200"
                status = v
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(200);
            } else {
                headers.push((k.to_string(), v.to_string()));
            }
        }
    }

    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        headers.push(("content-type".into(), "text/plain; charset=utf-8".into()));
    }

    Ok((status, headers, body.to_vec()))
}

/// Split `data` on the first blank line (`\r\n\r\n` or `\n\n`).
/// Returns (headers_slice, body_slice). If no blank line is found, treats
/// everything as a body with no custom headers (status 200 will be used).
fn split_on_blank_line(data: &[u8]) -> (&[u8], &[u8]) {
    if let Some(pos) = find_seq(data, b"\r\n\r\n") {
        return (&data[..pos], &data[pos + 4..]);
    }
    if let Some(pos) = find_seq(data, b"\n\n") {
        return (&data[..pos], &data[pos + 2..]);
    }
    (&data[..0], data)
}

fn find_seq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn error_response(status: StatusCode, msg: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from(msg)))
        .unwrap()
}
