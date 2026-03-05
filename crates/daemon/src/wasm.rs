/// Wasm executor using Wasmtime + WASI preview1.
///
/// Supports any binary compiled with GOOS=wasip1 (Go), wasm32-wasip1 (Rust/Zig),
/// or any other toolchain targeting WASI. Cold start is <1ms once the module
/// is compiled; compilation itself is ~10–50ms for typical binaries.
use anyhow::{Context, Result};
use wasmtime::*;
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

/// Execute a WASI Wasm binary synchronously in a Tokio blocking thread.
/// The module runs to completion (or panics/traps) before this returns.
pub async fn execute(wasm_path: &str, app_name: &str) -> Result<()> {
    let path = wasm_path.to_owned();
    let name = app_name.to_owned();
    tokio::task::spawn_blocking(move || run(&path, &name))
        .await
        .context("wasm executor thread panicked")?
}

fn run(wasm_path: &str, app_name: &str) -> Result<()> {
    let engine = Engine::default();

    // Linker with WASI preview1 host functions
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    preview1::add_to_linker_sync(&mut linker, |ctx| ctx)
        .context("adding WASI to linker")?;

    let wasm = std::fs::read(wasm_path)
        .with_context(|| format!("reading wasm binary: {}", wasm_path))?;
    let module = Module::new(&engine, &wasm)
        .context("compiling wasm module")?;

    let wasi = WasiCtxBuilder::new()
        .inherit_stdio()
        .build_p1();
    let mut store = Store::new(&engine, wasi);

    let instance = linker
        .instantiate(&mut store, &module)
        .context("instantiating wasm module")?;

    tracing::info!(app = app_name, path = wasm_path, "executing flash (wasm)");

    // WASI programs export `_start` as their main entry point
    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .context("wasm module must export _start")?;
    start.call(&mut store, ()).context("wasm execution failed")?;

    tracing::info!(app = app_name, "flash execution complete");
    Ok(())
}
