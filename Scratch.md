- **Billing** — the migration, Stripe integration end-to-end, invoice generation, failed payment handling. This is the biggest chunk you're missing.
- **Dashboard completeness** — the web templates for billing management, usage graphs, team management, service detail pages.
What it doesn't have that you DON'T need yet:**
- Rate limiting — HAProxy handles this today, and the API has its own rate limiter
- CDN/edge caching — your users deploy binaries that serve dynamic responses
- Sticky sessions — stateless services by design
- Multi-upstream load balancing — one slug = one VM/wasm instance
- OTLP tracing — nice-to-have, not blocking
