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
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::TcpListener;
use wasmtime::*;
use wasmtime_wasi::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::{I32Exit, WasiCtxBuilder};

/// Fuel budget per request — 1 billion units ≈ several seconds of CPU.
/// Override via WASM_FUEL env var.
static WASM_FUEL: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    common::config::env_or("WASM_FUEL", "1000000000")
        .parse()
        .unwrap_or(1_000_000_000)
});

/// Maximum Wasm stack depth. Override via WASM_STACK_BYTES env var.
static WASM_STACK_BYTES: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    common::config::env_or("WASM_STACK_BYTES", "1048576")
        .parse()
        .unwrap_or(1024 * 1024)
});

/// Maximum captured stdout size. Override via WASM_MAX_RESPONSE_BYTES env var.
static MAX_RESPONSE_BYTES: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    common::config::env_or("WASM_MAX_RESPONSE_BYTES", "4194304")
        .parse()
        .unwrap_or(4 * 1024 * 1024)
});

/// Default wall-clock timeout per Wasm request. Override via WASM_TIMEOUT_SECS.
const DEFAULT_WASM_TIMEOUT_SECS: u64 = 30;

/// Maximum linear memory per Wasm instance. Override via WASM_MAX_MEMORY_BYTES.
/// Default: 128 MiB. Prevents a malicious module from OOMing the daemon.
static WASM_MAX_MEMORY_BYTES: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    common::config::env_or("WASM_MAX_MEMORY_BYTES", "134217728")
        .parse()
        .unwrap_or(128 * 1024 * 1024)
});

/// Maximum concurrent requests per Wasm service. Override via WASM_MAX_CONCURRENT_REQUESTS.
static WASM_MAX_CONCURRENT: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    common::config::env_or("WASM_MAX_CONCURRENT_REQUESTS", "64")
        .parse()
        .unwrap_or(64)
});

/// How long to wait for a concurrency permit before returning 503.
static WASM_QUEUE_TIMEOUT: std::sync::LazyLock<std::time::Duration> =
    std::sync::LazyLock::new(|| {
        let secs: u64 = common::config::env_or("WASM_QUEUE_TIMEOUT_SECS", "5")
            .parse()
            .unwrap_or(5);
        std::time::Duration::from_secs(secs)
    });

/// Per-instance memory limiter for Wasm linear memory.
struct WasmLimiter {
    max_memory: usize,
}

impl ResourceLimiter for WasmLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool> {
        Ok(desired <= self.max_memory)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool> {
        Ok(desired <= 10_000)
    }
}

/// Store data: WASI context + resource limiter.
struct StoreData {
    wasi: WasiP1Ctx,
    limiter: WasmLimiter,
}

/// Pre-compiled Wasm service shared across all request-handling tasks.
struct WasmService {
    engine: Arc<Engine>,
    module: Arc<Module>,
    app_name: String,
    /// Per-request invocation counter, drained by the usage reporter every 60s.
    invocations: Arc<AtomicU64>,
    /// Wall-clock timeout per request.
    timeout: std::time::Duration,
    /// Concurrency limiter — prevents thread explosion under DDoS.
    concurrency: Arc<tokio::sync::Semaphore>,
    /// User-defined env vars (from `flux env set`), injected into every Wasm invocation.
    env_vars: std::collections::HashMap<String, String>,
}

/// Compile a wasm module, caching the native code to disk.
///
/// Cache key: `{wasm_path}.compiled` — a serialized Cranelift artifact keyed by
/// the SHA-256 of the original `.wasm` file (stored in the first 32 bytes).
/// On cache hit the module is deserialized in <10ms instead of 60-180s.
///
/// # Safety
/// `Module::deserialize` is `unsafe` because a corrupt/malicious cache file could
/// cause UB. We mitigate this by:
///   1. Storing a SHA-256 prefix and rejecting mismatches before deserializing.
///   2. The cache file lives in ARTIFACT_DIR which is daemon-owned.
async fn compile_or_cache(engine: &Arc<Engine>, wasm_path: &str, app_name: &str) -> Result<Module> {
    use sha2::{Digest, Sha256};

    let cache_path = format!("{wasm_path}.compiled");
    let wasm_bytes = tokio::fs::read(wasm_path)
        .await
        .with_context(|| format!("reading {wasm_path}"))?;

    let sha = Sha256::digest(&wasm_bytes);

    // Try cache hit — SHA prefix must match to guard against stale/corrupt cache.
    if let Ok(cached) = tokio::fs::read(&cache_path).await {
        if cached.len() > 32 && cached[..32] == sha[..] {
            tracing::info!(app = app_name, "wasm cache hit — deserializing");
            // SAFETY: cache file is daemon-owned, SHA-verified, and written by
            // Module::serialize from the same wasmtime version.
            match unsafe { Module::deserialize(engine, &cached[32..]) } {
                Ok(m) => return Ok(m),
                Err(e) => {
                    tracing::warn!(error = %e, "wasm cache deserialize failed — recompiling");
                    let _ = tokio::fs::remove_file(&cache_path).await;
                }
            }
        } else {
            tracing::info!(
                app = app_name,
                "wasm cache stale (SHA mismatch) — recompiling"
            );
            let _ = tokio::fs::remove_file(&cache_path).await;
        }
    }

    // Cache miss — full compilation on the blocking threadpool.
    tracing::info!(
        app = app_name,
        bytes = wasm_bytes.len(),
        "compiling wasm module (first time may take minutes for large Go binaries)"
    );
    let engine_clone = engine.clone();
    let module = tokio::task::spawn_blocking(move || Module::new(&engine_clone, &wasm_bytes))
        .await
        .context("wasm compile task panicked")?
        .context("compiling wasm module")?;

    // Serialize to cache: [32-byte SHA][serialized module]
    match module.serialize() {
        Ok(serialized) => {
            let mut cache_data = Vec::with_capacity(32 + serialized.len());
            cache_data.extend_from_slice(&sha);
            cache_data.extend_from_slice(&serialized);
            if let Err(e) = tokio::fs::write(&cache_path, &cache_data).await {
                tracing::warn!(error = %e, "failed to write wasm cache — next deploy will recompile");
            } else {
                tracing::info!(app = app_name, cache = cache_path, "wasm module cached");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Module::serialize failed — caching disabled for this module")
        }
    }

    Ok(module)
}

/// Compile `wasm_path`, bind a local TCP listener, and start serving requests.
/// Returns `(port, accept_task_handle)`. The port is written to the `services`
/// table as `upstream_addr`. The `JoinHandle` should be stored in `LiquidHandle`
/// so the accept loop can be aborted when the service is deprovisioned.
///
/// `invocations` is an externally-owned counter incremented on every request.
/// The usage reporter reads and resets it periodically to publish billing events.
pub async fn serve(
    wasm_path: String,
    app_name: String,
    invocations: Arc<AtomicU64>,
    env_vars: std::collections::HashMap<String, String>,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let mut cfg = Config::new();
    cfg.consume_fuel(true);
    cfg.max_wasm_stack(*WASM_STACK_BYTES);
    let engine = Arc::new(Engine::new(&cfg).context("wasmtime engine")?);

    let module = Arc::new(compile_or_cache(&engine, &wasm_path, &app_name).await?);

    let timeout_secs = std::env::var("WASM_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WASM_TIMEOUT_SECS);
    let timeout = std::time::Duration::from_secs(timeout_secs);

    // Bind on an OS-assigned port so we never collide across services.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding wasm HTTP listener")?;
    let port = listener.local_addr()?.port();

    let concurrency = Arc::new(tokio::sync::Semaphore::new(*WASM_MAX_CONCURRENT));
    tracing::info!(
        app = app_name,
        port,
        timeout_secs,
        max_concurrent = *WASM_MAX_CONCURRENT,
        max_memory_bytes = *WASM_MAX_MEMORY_BYTES,
        "liquid wasm HTTP shim ready"
    );

    let svc = Arc::new(WasmService {
        engine,
        module,
        app_name,
        invocations,
        timeout,
        concurrency,
        env_vars,
    });

    let accept_task = tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    tracing::error!(error = %e, "accept error");
                    continue;
                }
            };

            let svc = svc.clone();
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
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

    Ok((port, accept_task))
}

/// Handle a single HTTP request: extract context, run Wasm, return response.
async fn dispatch(
    svc: Arc<WasmService>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    // Acquire concurrency permit — prevents thread explosion under load.
    let _permit =
        match tokio::time::timeout(*WASM_QUEUE_TIMEOUT, svc.concurrency.clone().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                // Semaphore closed — shouldn't happen in normal operation.
                return Ok(error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "service shutting down".into(),
                ));
            }
            Err(_) => {
                tracing::warn!(
                    app = svc.app_name,
                    "wasm concurrency limit reached — rejecting request"
                );
                return Ok(error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "too many concurrent requests".into(),
                ));
            }
        };

    svc.invocations.fetch_add(1, Ordering::Relaxed);
    let method = req.method().to_string();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("").to_string();

    // Build WAGI/CGI env vars from the request.
    let mut env_vars: Vec<(String, String)> = vec![
        ("REQUEST_METHOD".into(), method),
        ("PATH_INFO".into(), path),
        ("QUERY_STRING".into(), query),
        ("SERVER_PROTOCOL".into(), "HTTP/1.1".into()),
    ];

    for (k, v) in req.headers() {
        let name = k.as_str();
        let val = v.to_str().unwrap_or("").to_string();
        if name.eq_ignore_ascii_case("content-type") {
            env_vars.push(("CONTENT_TYPE".into(), val.clone()));
        }
        let env_key = format!("HTTP_{}", name.to_uppercase().replace('-', "_"));
        env_vars.push((env_key, val));
    }

    let body_bytes: Vec<u8> = match req.into_body().collect().await {
        Ok(b) => b.to_bytes().to_vec(),
        Err(_) => vec![],
    };
    env_vars.push(("CONTENT_LENGTH".into(), body_bytes.len().to_string()));

    // Inject user-defined env vars (from `flux env set`).
    for (k, v) in &svc.env_vars {
        env_vars.push((k.clone(), v.clone()));
    }

    let engine = svc.engine.clone();
    let module = svc.module.clone();
    let app_name = svc.app_name.clone();

    let timeout = svc.timeout;
    let result = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || {
            invoke(&engine, &module, &app_name, env_vars, body_bytes)
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok((status, headers, body)))) => {
            let mut builder = Response::builder().status(status);
            for (k, v) in headers {
                builder = builder.header(k, v);
            }
            Ok(builder.body(Full::new(Bytes::from(body))).unwrap())
        }
        Ok(Ok(Err(e))) => {
            tracing::error!(error = %e, app = svc.app_name, "wasm execution error");
            Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                e.to_string(),
            ))
        }
        Ok(Err(e)) => {
            tracing::error!(error = ?e, "wasm task panicked");
            Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".into(),
            ))
        }
        Err(_) => {
            tracing::error!(
                app = svc.app_name,
                timeout_secs = timeout.as_secs(),
                "wasm execution timed out"
            );
            Ok(error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "wasm execution timed out".into(),
            ))
        }
    }
}

/// Synchronous Wasm invocation — runs in a blocking thread pool task.
fn invoke(
    engine: &Engine,
    module: &Module,
    app_name: &str,
    env_vars: Vec<(String, String)>,
    body: Vec<u8>,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut linker: Linker<StoreData> = Linker::new(engine);
    preview1::add_to_linker_sync(&mut linker, |data| &mut data.wasi)
        .context("adding WASI preview1 to linker")?;

    // Capture stdout — shared handle so we can read bytes after execution.
    let stdout = MemoryOutputPipe::new(*MAX_RESPONSE_BYTES);
    let stdin = MemoryInputPipe::new(Bytes::from(body));

    let env_pairs: Vec<(&str, &str)> = env_vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let wasi = WasiCtxBuilder::new()
        .envs(&env_pairs)
        .stdin(stdin)
        .stdout(stdout.clone())
        .build_p1();

    let data = StoreData {
        wasi,
        limiter: WasmLimiter {
            max_memory: *WASM_MAX_MEMORY_BYTES,
        },
    };

    let mut store = Store::new(engine, data);
    store.limiter(|data| &mut data.limiter);
    store.set_fuel(*WASM_FUEL).context("setting wasm fuel")?;

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
            if err
                .downcast_ref::<I32Exit>()
                .map(|e| e.0 == 0)
                .unwrap_or(false)
            {
                // clean exit — fall through
            } else {
                return Err(err.context("wasm execution failed"));
            }
        }
    }

    let output = stdout.contents().to_vec();
    tracing::debug!(
        app = app_name,
        response_bytes = output.len(),
        "wasm request done"
    );

    parse_wagi_response(output)
}

/// Parse CGI/WAGI response written by the Wasm module to stdout.
fn parse_wagi_response(output: Vec<u8>) -> Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let (header_bytes, body) = split_on_blank_line(&output);

    let header_str =
        std::str::from_utf8(header_bytes).context("response headers are not valid UTF-8")?;

    let mut status: u16 = 200;
    let mut headers: Vec<(String, String)> = Vec::new();

    for line in header_str.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
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

    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
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
