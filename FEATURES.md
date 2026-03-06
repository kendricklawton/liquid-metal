# Liquid Metal — Feature Roadmap (Solo Dev)

Everything in this file is realistically buildable by one person. Nothing here requires a team,
a managed cloud, or infrastructure you don't already own.

---

## Core Platform (Ship First)

These are table-stakes — users expect them on day one.

| Feature | Engine | Where It Lives |
|---|---|---|
| `flux deploy` — binary + .wasm upload | Metal + Liquid | CLI → API → daemon |
| `flux logs <id>` — tail service logs | Metal + Liquid | daemon → NATS → API → CLI |
| `flux status` — list services + health | Metal + Liquid | API → CLI |
| Custom domains + auto TLS | Both | Pingora — ACME via Let's Encrypt |
| Encrypted secrets (`machine.toml` → env) | Both | API encrypts at rest, daemon injects |
| Service pause / resume | Both | API sets status, daemon respects it |
| Rollback to previous deploy | Both | Object Storage — `deploy_id` (uuid_v7) is immutable |
| Deploy history per service | Both | `services` table + deploy rows |
| Usage dashboard (credits burned, invocations) | Both | `usage_records` table → web UI |

---

## Billing & Metering

All of this is math on top of data you already collect.

| Feature | Notes |
|---|---|
| Compute hours (Metal) | daemon records `started_at` / `stopped_at` per VM |
| Wasm invocations (Liquid) | daemon increments counter per `wasmtime::execute` |
| Provisioned memory GB-hrs | `memory_mb * active_seconds / 3600` — write to `usage_records` |
| Egress bandwidth | eBPF TC classifier already planned — sum bytes per service |
| Prepaid credit balance | `workspaces.credit_balance` — deduct on usage flush (every 60s) |
| Manual top-up (dashboard) | Stripe PaymentIntent → credit ledger entry |
| Auto-recharge (opt-in) | Stripe off-session charge when balance < threshold |
| Hard pause at zero credits | daemon checks balance before boot; API rejects new deploys |
| Pro: $10 credit included / month | Cron job adds $10 to balance on billing cycle |
| Team: $20 credit per seat / month | Cron job adds `$20 * seat_count` to workspace pool |

---

## Cron Jobs

Natural fit for both engines — especially Liquid.

| Feature | Notes |
|---|---|
| Cron schedule in `machine.toml` | `[cron] schedule = "0 * * * *"` |
| Liquid cron | NATS JetStream scheduled consumer fires `wasm_invoke` message |
| Metal cron | daemon wakes sleeping VM on schedule, runs, sleeps again |
| Cron management in dashboard | list, pause, trigger manually |
| Cron run history + last result | `cron_runs` table — status, duration, exit code |

---

## Configurable Routing

Already mostly there — Pingora does this natively.

| Feature | Notes |
|---|---|
| Reverse proxy (slug → upstream_addr) | **Done** |
| Path rewrites | `routing_rules` table — `{match, rewrite}` — Pingora reads on request |
| Redirects (301/302) | Same rule engine, different action type |
| Custom domain routing | Pingora SNI → domain → service lookup |
| Port routing per service | `machine.toml` declares port, stored in `services.port` |
| Middleware (header injection) | Pingora `upstream_request_filter` — inject headers from `service_env_vars` |

---

## Security — What's Actually Buildable

### Do build

| Feature | Tier | Notes |
|---|---|---|
| IP blocking (up to 100 IPs) | Pro + Team | `ip_blocklist` table — Pingora checks on connect. Dashboard UI to manage. |
| Rate limiting per service | Pro + Team | Token bucket in Pingora memory (per IP + per service). Config: `req/min`. |
| Custom firewall rules (up to 40) | Pro + Team | `firewall_rules` table — `{source_ip_cidr, path_pattern, method, action}`. Pingora evaluator. |
| Allowlist / bypass rules | Team | Same rule engine, allowlist direction. Useful for IP-locked staging. |
| Attack challenge mode (basic) | All | Pingora serves a JS proof-of-work page when `challenge_mode = true` on a service. Handles unsophisticated floods. |
| DDoS basic mitigation | All | Rate limiting + IP blocking + Vultr upstream scrubbing. Good enough for 99% of attacks on a small PaaS. |

### Don't build (skip or embed)

| Feature | Why |
|---|---|
| OWASP Core Ruleset (managed) | Embed [Coraza](https://coraza.io) as a Pingora plugin — get the CRS for free without owning it |
| AI bot detection | Requires ML pipelines + threat intel feeds. Not your differentiator. |
| Managed bot rulesets | Same — full-time maintenance job. Skip. |
| BGP-level DDoS scrubbing | Requires upstream ISP partnerships. Lean on Vultr's scrubbing center instead. |

---

## Team & Workspace

| Feature | Notes |
|---|---|
| Workspace roles: owner, member, viewer | `workspace_members.role` enum — already in schema |
| Viewer roles always free | billing only counts owner + member seats |
| Shared credit pool | `workspaces.credit_balance` — all services in workspace draw from one pool |
| Audit log | `audit_log` table — already partitioned in schema |
| Invite by email | WorkOS org invites → provision `workspace_members` row |
| Seat count billing | `stripe_subscription` with `quantity = active_seat_count` |

---

## Observability

Minimal but useful — you don't need Datadog.

| Feature | Notes |
|---|---|
| Log streaming (`flux logs`) | daemon → NATS subject `logs.{service_id}` → SSE endpoint → CLI |
| Build log lines | `build_log_lines` table — already partitioned in schema |
| Service status history | `status_events` table — provisioning / running / stopped / failed |
| Basic metrics (invocations/hr, p50 boot time) | Aggregate from `usage_records` — no external metrics store needed |
| Uptime / health probe | Pingora pings `upstream_addr/healthz` every 30s — writes to `services.last_health_check` |

---

## CLI (`flux`)

| Command | Status |
|---|---|
| `flux login` | Done |
| `flux whoami` | Done |
| `flux deploy` | Done |
| `flux status` | Done |
| `flux logs <id>` | Done |
| `flux rollback <id> <deploy_id>` | TODO |
| `flux secrets set KEY=VALUE` | TODO |
| `flux secrets list` | TODO |
| `flux domains add <domain>` | TODO |
| `flux cron list` | TODO |

---

## What Makes This Defensible Against Vercel/Render

These are things they cannot offer — lean into them.

| Differentiator | Why it matters |
|---|---|
| Declared vCPU + RAM in `machine.toml` | No mystery resource pools. You get exactly what you asked for. |
| Firecracker hardware isolation | Not a container. A real KVM guest. Security story is stronger. |
| Wasm sub-millisecond cold start | No warm instances needed. Pay per invocation at near-zero cost. |
| Bare metal bandwidth (10 Gbps) | Vercel charges per GB. You can offer generous transfer limits cheaply. |
| Transparent credit billing | No surprise overages. Services pause cleanly at zero. |
| No Kubernetes anywhere | Simpler ops, lower overhead, lower costs passed to users. |
