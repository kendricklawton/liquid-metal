# Liquid Metal — Runbook

Operational reference for local development. Follow top to bottom on a fresh session.

---

## Prerequisites (one-time)

### Tools
```bash
brew install go rustup go-task goreleaser
rustup default stable
```

### Go PATH (add to ~/.zshrc permanently)
```bash
echo 'export PATH="$PATH:$(go env GOPATH)/bin"' >> ~/.zshrc
source ~/.zshrc
```

### Install the flux CLI (rebuild after CLI changes)
```bash
cd ~/repos/liquid-metal/cli
go build -o $(go env GOPATH)/bin/flux .
flux --help  # verify
```

---

## Starting the Dev Environment

Open 3 terminal tabs.

### Tab 1 — Infrastructure (Docker)
```bash
cd ~/repos/liquid-metal
task up
```
Starts: Postgres (:5432), NATS (:4222), RustFS S3 mock (:9000, console :9001)

### Tab 2 — Rust API
```bash
cd ~/repos/liquid-metal
task dev:api
```
Waits for: `Listening on 0.0.0.0:7070`

### Tab 3 — Daemon
```bash
cd ~/repos/liquid-metal
task dev:daemon
```
Waits for: `TAP counter initialized from DB`

> On macOS: Firecracker/TAP are skipped automatically. Only Liquid (Wasm) deployments work locally.

---

## Authenticate

```bash
flux login
# Opens browser → WorkOS → token saved to ~/.config/flux/config.yaml
```

Verify:
```bash
flux whoami
```

---

## Deploy a Service (Liquid/Wasm)

Using the markdown-renderer example:

```bash
cd ~/repos/liquid-metal-templates/go/liquid/markdown-renderer
flux init      # creates project in API, writes project_id into liquid-metal.toml
flux deploy    # builds main.wasm → uploads to RustFS → API → NATS → daemon runs it
```

Expected deploy output:
```
=> Deploying markdown-renderer (Engine: Liquid)...
=> Compiling Go to WebAssembly (wasip1)...
=> Artifact built: main.wasm (SHA256: xxxxxxxx...)
=> Requesting upload destination...
=> Uploading artifact to object storage...
=> Finalizing deployment...

✅ Deployment Successful!
   Service: markdown-renderer
   Status:  SERVICE_STATUS_PROVISIONING
```

---

## Verify the Service is Running

```bash
flux status
```

Expected output (after daemon provisions it):
```
NAME                ENGINE   STATUS    UPSTREAM
markdown-renderer   liquid   running   127.0.0.1:XXXXX
```

> If status stays `provisioning` for more than a few seconds, check Tab 3 (daemon logs) for errors.

---

## Hit the Service

```bash
# Replace PORT with the value from flux status UPSTREAM column
curl http://127.0.0.1:PORT/
# → returns HTMX live-preview HTML form

curl -X POST http://127.0.0.1:PORT/ \
  --data-binary "# Hello\n\n**world**"
# → returns rendered HTML
```

---

## Tail Logs

```bash
# Get the service ID from flux status
flux status

# Tail logs (last 100 lines by default)
flux logs <service-id>
flux logs <service-id> --limit 500
```

---

## Workspace & Project Management

```bash
flux workspace list          # list workspaces (* = active)
flux workspace use <slug>    # switch active workspace

flux project list            # list projects in active workspace (* = current dir's project)
flux project use <slug>      # set project_id in ./liquid-metal.toml
```

---

## Stopping the Dev Environment

```bash
# Stop docker services
cd ~/repos/liquid-metal
task down

# Kill API and daemon with Ctrl+C in their respective terminals
```

---

## Re-deploying After Code Changes

```bash
# After changing CLI source
cd ~/repos/liquid-metal/cli
go build -o $(go env GOPATH)/bin/flux .

# After changing API or daemon source — restart the relevant tab (Ctrl+C, then task dev:api / task dev:daemon)

# Re-deploy a service
cd ~/repos/liquid-metal-templates/go/liquid/markdown-renderer
flux deploy    # liquid-metal.toml already has project_id from flux init
```

---

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `flux: command not found` | `export PATH="$PATH:$(go env GOPATH)/bin"` and rebuild |
| `flux login` fails | `FLUX_WORKOS_CLIENT_ID` in `.env`, confirm `task up` is running |
| `flux init` fails | API running? (`task dev:api`) |
| `flux deploy` upload fails | RustFS running? (`task up`) Check `http://localhost:9001` |
| Status stuck at `provisioning` | Check daemon tab for errors |
| `GetUploadUrl` error | API can't reach RustFS — check `.env` S3 vars |
