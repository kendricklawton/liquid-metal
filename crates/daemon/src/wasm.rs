/// Wasm executor using Wasmtime + WASI preview1.
///
/// Security properties:
///   - Linear memory isolation: Wasm has no pointers to host memory.
///     A Wasm module physically cannot read daemon memory, NATS tokens,
///     or the Postgres connection string — the address space is separate.
///   - Fuel metering: every CPU instruction costs 1 unit of "fuel".
///     When the budget is exhausted Wasmtime traps and the execution ends.
///     This prevents infinite-loop DoS from any tenant's Wasm binary.
///   - Stack depth: bounded by `max_wasm_stack` in the Engine config.
///
/// Supports any binary compiled with GOOS=wasip1 (Go), wasm32-wasip1
/// (Rust/Zig), or any other WASI-targeting toolchain.
use anyhow::{Context, Result};
use wasmtime::*;
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

/// Fuel budget per invocation.
/// 1 billion ≈ several seconds of CPU on modern hardware — generous for a
/// function, but bounded. Tune down for stricter tiers.
const WASM_FUEL: u64 = 1_000_000_000;

/// Maximum Wasm stack depth in bytes (1 MiB).
const WASM_STACK_BYTES: usize = 1024 * 1024;

/// Execute a WASI Wasm binary synchronously in a Tokio blocking thread.
/// The module runs to completion (or traps/runs out of fuel) before returning.
pub async fn execute(wasm_path: &str, app_name: &str) -> Result<()> {
    let path = wasm_path.to_owned();
    let name = app_name.to_owned();
    tokio::task::spawn_blocking(move || run(&path, &name))
        .await
        .context("wasm executor thread panicked")?
}

fn run(wasm_path: &str, app_name: &str) -> Result<()> {
    // Fuel metering + stack depth limit
    let mut config = Config::new();
    config.consume_fuel(true);
    config.max_wasm_stack(WASM_STACK_BYTES);
    let engine = Engine::new(&config).context("creating wasmtime engine")?;

    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    preview1::add_to_linker_sync(&mut linker, |ctx| ctx)
        .context("adding WASI to linker")?;

    let wasm = std::fs::read(wasm_path)
        .with_context(|| format!("reading wasm binary: {}", wasm_path))?;
    let module = Module::new(&engine, &wasm)
        .context("compiling wasm module")?;

    let wasi = WasiCtxBuilder::new().inherit_stdio().build_p1();
    let mut store = Store::new(&engine, wasi);

    // Grant the fuel budget — exhausting it causes a deterministic trap,
    // not a panic or hang.
    store.set_fuel(WASM_FUEL).context("setting wasm fuel")?;

    let instance = linker
        .instantiate(&mut store, &module)
        .context("instantiating wasm module")?;

    tracing::info!(app = app_name, path = wasm_path, fuel = WASM_FUEL, "executing flash (wasm)");

    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .context("wasm module must export _start")?;

    start.call(&mut store, ()).context("wasm execution failed")?;

    let fuel_remaining = store.get_fuel().unwrap_or(0);
    tracing::info!(
        app = app_name,
        fuel_used = WASM_FUEL - fuel_remaining,
        fuel_remaining,
        "flash execution complete"
    );
    Ok(())
}
