Phase | What | Depends on | Risk |
|---|---|---|---|
| **1. Snapshot create** | Pause VM after startup probe, snapshot to S3, halt VM | Nothing (builds on rootfs work) | Low |
| **2. Wake-on-request** | Proxy holds request, publishes WakeEvent, daemon restores from snapshot | Phase 1 | High (proxy buffering is new) |
| **3. Drop CPU pinning** | Remove cpuset + SMT offlining, let scheduler float | Phase 1+2 working | Low |
| **4. Per-invocation billing** | Count requests in proxy, replace vCPU-hour billing | Phase 2 (proxy changes) | Medium (billing migration)
