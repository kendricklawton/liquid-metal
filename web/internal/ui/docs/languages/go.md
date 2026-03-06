# Go

Liquid Metal builds Go services as static binaries with `CGO_ENABLED=0`. No Dockerfile, no base image selection — just your code compiled to machine code running in a scratch container.

## Detection

A project is detected as Go when a `go.mod` file is present at the root.

## Build

```bash
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags="-s -w" -o /app ./...
```

The `-s -w` flags strip the symbol table and DWARF debug info, reducing binary size.

## Runtime tuning

The platform automatically sets:

```bash
GOMEMLIMIT=<container_memory_limit * 0.9>
GOGC=100
GOMAXPROCS=<vcpu_count>
```

You can override these via [Secrets & Environment Variables](/docs/secrets).

## pprof profiling

If your service imports `net/http/pprof`, the platform exposes the pprof endpoint on an internal port and streams profiles to the dashboard.

```go
import _ "net/http/pprof"
```

> **Tip:** The pprof endpoint is never exposed on your public port. It binds to an internal sidecar address only accessible from the dashboard.

## Race detector builds

Preview environments automatically compile with `-race` to surface data races before code reaches `main`.

```bash
# The platform runs this in preview environments:
go build -race -o /app ./...
```

## Example `plat.toml`

```toml
[build]
lang = "go"
main = "./cmd/api"      # path to main package (default: "./...")

[serve]
port = 8080
health_path = "/healthz"
```
