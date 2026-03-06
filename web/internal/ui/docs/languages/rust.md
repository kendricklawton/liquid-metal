# Rust

Liquid Metal builds Rust services targeting `x86_64-unknown-linux-musl` — fully static binaries with zero system dependencies, no libc, no runtime overhead.

## Detection

A project is detected as Rust when a `Cargo.toml` file is present at the root.

## Build

```bash
cargo build --release --target x86_64-unknown-linux-musl
```

The platform installs the musl target automatically. LTO and strip are enabled by default via injected `Cargo.toml` profile settings:

```toml
[profile.release]
lto = true
strip = true
opt-level = 3
codegen-units = 1
```

## No GC. No runtime.

Rust has no garbage collector. Memory is freed the instant ownership ends — determined entirely at compile time. The borrow checker enforces this at compile time, not at runtime.

> **Tip:** This is the key reason static Rust binaries are so small and fast. There is no runtime heap allocator on your critical path.

## Async runtime

The platform is async-aware. If your `Cargo.toml` depends on `tokio`, the worker thread pool is sized automatically based on available vCPUs:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
axum = "0.7"
```

```rust
#[tokio::main]
async fn main() {
    // tokio runtime is configured by the platform
}
```

## Dependency auditing

Every push runs `cargo audit` against the OSV database. If a CVE is found in your dependency tree, the deploy is flagged (not blocked — you choose whether to gate on it).

## Example `plat.toml`

```toml
[build]
lang = "rust"
target = "x86_64-unknown-linux-musl"   # default

[serve]
port = 3000
health_path = "/health"
```
