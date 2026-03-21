# Interview Prep — Honeywell FM&T App Development Analyst II

---

# Part 1: ITIL Foundation

ITIL (Information Technology Infrastructure Library) is a framework for managing IT services. Honeywell FM&T explicitly requires "fundamental ITIL processes." This section covers everything you need to speak to confidently.

---

## 1.1 What ITIL Actually Is

ITIL is not a technology — it's a set of practices for aligning IT services with business needs. At a DOE manufacturing facility, "IT services" means the internal applications that support production: requirements tracking (DOORS), product lifecycle management (Windchill/Enovia), manufacturing execution (Solumina), and custom tools the dev team builds.

The current version is ITIL 4 (2019), which organizes everything around the **Service Value System (SVS)** — but interviewers at this level will mostly ask about the core practices, not the meta-framework.

## 1.2 The Service Value Chain

The SVS has six activities. You don't need to memorize these, but understand the flow:

```
Plan → Improve → Engage → Design & Transition → Obtain/Build → Deliver & Support
```

As a developer, you live in **Obtain/Build** (writing code) and **Deliver & Support** (keeping it running). But you interact with all of them.

## 1.3 The Four Dimensions of Service Management

Every service has four dimensions. When they ask "how do you think about building a new feature?" — frame your answer around these:

1. **Organizations & People** — Who uses it? Who maintains it? What skills are needed?
2. **Information & Technology** — What data does it need? What systems does it integrate with?
3. **Partners & Suppliers** — What third parties are involved? (PTC for Windchill, IBM for DOORS, etc.)
4. **Value Streams & Processes** — What's the workflow from request to delivery?

## 1.4 Core ITIL Practices (Interview-Critical)

### Incident Management

**What it is:** Restoring normal service operation as quickly as possible after an unplanned interruption.

**Key concepts:**
- **Incident** — an unplanned interruption to a service (app is down, feature is broken, data is wrong)
- **Major incident** — high-impact incident requiring a dedicated response team
- **Incident priority** = Impact x Urgency
  - Impact: how many users/processes affected
  - Urgency: how quickly a resolution is needed
- **Escalation** — functional (to a specialist) or hierarchical (to management)

**The lifecycle:**
```
Detection → Logging → Categorization → Prioritization → Diagnosis → Resolution → Closure
```

**What to say in the interview:**
"When an incident comes in, I log it with enough detail to reproduce, categorize it by affected system, set priority based on impact and urgency, then work the diagnosis. If I can't resolve it within the SLA window, I escalate — I don't sit on it. After resolution, I document the root cause and whether it needs a problem record for deeper investigation."

**Your experience mapping:**
- Your RUNBOOK.md IS incident management documentation
- Your Grafana alerts are detection
- Your backup/restore procedures are resolution plans

---

### Problem Management

**What it is:** Identifying and addressing the root cause of incidents to prevent recurrence.

**Key difference from incidents:** Incidents are about restoring service NOW. Problems are about understanding WHY it happened and preventing it from happening again.

**Key concepts:**
- **Problem** — the underlying cause of one or more incidents
- **Known error** — a problem with a documented root cause and workaround
- **Root Cause Analysis (RCA)** — techniques like 5 Whys, fishbone diagram, fault tree analysis
- **Workaround** — a temporary fix that reduces impact while the permanent fix is developed

**The lifecycle:**
```
Problem Detection → Logging → Investigation → Diagnosis (RCA) → Resolution → Closure
```

**What to say:**
"I distinguish between fixing the symptom and fixing the cause. If the same type of incident keeps recurring, I open a problem record, do root cause analysis, and either fix it permanently or document a known error with a workaround until we can. At Clyde & Co, when our test automation kept failing due to enterprise proxy issues, I didn't just retry — I diagnosed the root cause (CDP connection blocked by proxy) and built a framework that bypassed it entirely."

---

### Change Management (Change Enablement in ITIL 4)

**What it is:** Controlling the lifecycle of all changes to minimize disruption.

**Key concepts:**
- **Change** — any addition, modification, or removal of anything that could affect IT services
- **Change types:**
  - **Standard** — pre-approved, low-risk, follows a documented procedure (e.g., applying a patch you've applied before)
  - **Normal** — requires assessment and authorization through the change process
  - **Emergency** — must be implemented urgently, post-implementation review required
- **Change Advisory Board (CAB)** — group that evaluates and authorizes normal changes
- **Post-Implementation Review (PIR)** — did the change achieve its objective? Any side effects?

**The lifecycle:**
```
Request for Change (RFC) → Assessment → Authorization → Implementation → Review → Closure
```

**What to say:**
"Every change to production goes through a process — I don't just push code. I assess the risk, document what's changing and why, get approval if needed, implement with a rollback plan, and review after deployment. In my Liquid Metal project, I use Terraform for infrastructure changes (plan before apply), Nomad for application deploys (rolling updates with health checks), and GitHub Actions for CI/CD with automated testing gates."

**At a DOE facility:** Change management is SERIOUS. They handle national security components. Every change to production systems likely requires formal approval, documentation, and audit trail. Emphasize your discipline around this.

---

### Service Request Management

**What it is:** Handling routine user requests that are NOT incidents.

**Examples at a manufacturing facility:**
- "I need access to the DOORS project for the new program"
- "Can you add a new field to the work order form?"
- "I need a report showing all parts that failed QA this month"

**Key concepts:**
- **Service catalog** — the menu of available services/requests
- **Fulfillment** — completing the request
- **SLA** — agreed timeline for fulfillment

**What to say:**
"I differentiate between incidents (something's broken) and service requests (someone needs something). Service requests follow a defined fulfillment process — they're not emergencies, they're planned work. I prioritize them alongside project work and track them to completion."

---

### Configuration Management

**What it is:** Tracking all the components (Configuration Items / CIs) that make up your IT services.

**Key concepts:**
- **CI (Configuration Item)** — any component that needs to be managed: server, application, database, network device, document
- **CMDB (Configuration Management Database)** — the authoritative record of all CIs and their relationships
- **Baseline** — a snapshot of a configuration at a point in time

**Your experience mapping:**
- Your Terraform state IS a CMDB for infrastructure
- Your Cargo.lock IS a configuration baseline for dependencies
- Your ARCHITECTURE.md documents the relationships between CIs

---

### Release Management

**What it is:** Planning, scheduling, and controlling the movement of releases to production.

**Key concepts:**
- **Release** — a collection of changes deployed together
- **Release package** — the artifacts, documentation, and procedures for a release
- **Deployment pipeline** — the automated path from code to production

**Your experience mapping:**
- cargo-dist builds release artifacts across 5 platforms
- GitHub Actions runs the pipeline (ci.yml → release.yml → deploy.yml)
- Nomad does rolling deploys with health checks

---

### Continual Improvement

**What it is:** Always looking for ways to make services better.

**The Continual Improvement Model:**
```
1. What is the vision? (business objectives)
2. Where are we now? (current state assessment)
3. Where do we want to be? (measurable targets)
4. How do we get there? (improvement plan)
5. Take action (implement)
6. Did we get there? (measure results)
7. How do we keep the momentum? (embed in culture)
```

**What to say:**
"I maintain a living TODO with concrete improvement tasks prioritized by business impact. I don't improve for improvement's sake — I improve what's blocking users or creating risk."

---

## 1.5 ITIL Vocabulary Quick Reference

| Term | Definition |
|------|-----------|
| SLA | Service Level Agreement — measurable commitment (e.g., "99.9% uptime") |
| SLO | Service Level Objective — the target within an SLA |
| SLI | Service Level Indicator — the metric that measures an SLO |
| MTTR | Mean Time to Restore — average time to fix an incident |
| MTBF | Mean Time Between Failures — average time between incidents |
| RFC | Request for Change — formal change proposal |
| CAB | Change Advisory Board — approves/rejects changes |
| CI | Configuration Item — any managed component |
| CMDB | Configuration Management Database |
| PIR | Post-Implementation Review |
| RCA | Root Cause Analysis |
| KEDB | Known Error Database |

---

# Part 2: SQL Deep Dive

Honeywell lists SQL as a core competency. You write raw SQL daily (tokio-postgres). This section covers what they'll likely whiteboard-test you on.

---

## 2.1 Fundamentals They'll Assume You Know

### SELECT with filtering and sorting

```sql
-- All parts that failed QA in the last 30 days, newest first
SELECT part_number, defect_type, inspector, inspected_at
FROM qa_inspections
WHERE result = 'fail'
  AND inspected_at > NOW() - INTERVAL '30 days'
ORDER BY inspected_at DESC;
```

### INSERT, UPDATE, DELETE

```sql
-- Insert a new work order
INSERT INTO work_orders (id, part_number, quantity, status, created_by, created_at)
VALUES (gen_random_uuid(), 'PN-4421', 100, 'pending', 'klawton', NOW());

-- Update status
UPDATE work_orders
SET status = 'in_progress', started_at = NOW()
WHERE id = '550e8400-e29b-41d4-a716-446655440000';

-- Soft delete (DOE facilities never hard delete)
UPDATE work_orders
SET deleted_at = NOW(), deleted_by = 'klawton'
WHERE id = '550e8400-e29b-41d4-a716-446655440000';
```

---

## 2.2 JOINs

This is the #1 thing they'll whiteboard. Know all four.

### INNER JOIN — only matching rows

```sql
-- Work orders with their part details
SELECT wo.id, wo.quantity, wo.status, p.part_number, p.description
FROM work_orders wo
INNER JOIN parts p ON wo.part_id = p.id
WHERE wo.status = 'in_progress';
```

### LEFT JOIN — all from left, matching from right (NULL if no match)

```sql
-- All parts, even those with no work orders
SELECT p.part_number, p.description, wo.id AS work_order_id, wo.status
FROM parts p
LEFT JOIN work_orders wo ON p.id = wo.part_id;

-- Find parts that have NEVER had a work order
SELECT p.part_number, p.description
FROM parts p
LEFT JOIN work_orders wo ON p.id = wo.part_id
WHERE wo.id IS NULL;
```

### RIGHT JOIN — all from right, matching from left

```sql
-- All inspectors, even those who haven't inspected anything
SELECT i.name, COUNT(qi.id) AS inspections
FROM qa_inspections qi
RIGHT JOIN inspectors i ON qi.inspector_id = i.id
GROUP BY i.name;
```

### FULL OUTER JOIN — all rows from both sides

```sql
-- All parts and all work orders, matched where possible
SELECT p.part_number, wo.id AS work_order_id
FROM parts p
FULL OUTER JOIN work_orders wo ON p.id = wo.part_id;
```

### Self JOIN — a table joined to itself

```sql
-- Find parts that share the same supplier
SELECT a.part_number AS part_a, b.part_number AS part_b, a.supplier_id
FROM parts a
INNER JOIN parts b ON a.supplier_id = b.supplier_id AND a.id < b.id;
```

### Multi-table JOIN

```sql
-- Work order with part details, assigned operator, and latest QA result
SELECT
  wo.id,
  p.part_number,
  p.description,
  op.name AS operator,
  qi.result AS last_qa_result
FROM work_orders wo
JOIN parts p ON wo.part_id = p.id
JOIN operators op ON wo.operator_id = op.id
LEFT JOIN qa_inspections qi ON wo.id = qi.work_order_id
  AND qi.inspected_at = (
    SELECT MAX(inspected_at)
    FROM qa_inspections
    WHERE work_order_id = wo.id
  );
```

---

## 2.3 GROUP BY and Aggregation

```sql
-- Defect counts by type this month
SELECT defect_type, COUNT(*) AS count
FROM qa_inspections
WHERE result = 'fail'
  AND inspected_at >= DATE_TRUNC('month', CURRENT_DATE)
GROUP BY defect_type
ORDER BY count DESC;

-- Pass rate per inspector
SELECT
  inspector,
  COUNT(*) AS total,
  COUNT(*) FILTER (WHERE result = 'pass') AS passed,
  ROUND(
    100.0 * COUNT(*) FILTER (WHERE result = 'pass') / COUNT(*),
    1
  ) AS pass_rate_pct
FROM qa_inspections
GROUP BY inspector
ORDER BY pass_rate_pct DESC;
```

### HAVING — filter on aggregated results

```sql
-- Inspectors with pass rate below 90%
SELECT
  inspector,
  ROUND(100.0 * COUNT(*) FILTER (WHERE result = 'pass') / COUNT(*), 1) AS pass_rate
FROM qa_inspections
GROUP BY inspector
HAVING (100.0 * COUNT(*) FILTER (WHERE result = 'pass') / COUNT(*)) < 90
ORDER BY pass_rate;
```

---

## 2.4 Subqueries

### Scalar subquery (returns one value)

```sql
-- Parts with more defects than average
SELECT part_number, defect_count
FROM parts
WHERE defect_count > (SELECT AVG(defect_count) FROM parts);
```

### IN subquery (returns a list)

```sql
-- Work orders for parts supplied by "Acme Corp"
SELECT wo.*
FROM work_orders wo
WHERE wo.part_id IN (
  SELECT id FROM parts WHERE supplier_name = 'Acme Corp'
);
```

### EXISTS subquery (true/false)

```sql
-- Parts that have at least one failed inspection
SELECT p.part_number, p.description
FROM parts p
WHERE EXISTS (
  SELECT 1 FROM qa_inspections qi
  WHERE qi.part_id = p.id AND qi.result = 'fail'
);
```

### Correlated subquery (references outer query)

```sql
-- Each part with its most recent inspection date
SELECT
  p.part_number,
  (SELECT MAX(inspected_at)
   FROM qa_inspections qi
   WHERE qi.part_id = p.id) AS last_inspected
FROM parts p;
```

---

## 2.5 Window Functions

Window functions are the advanced topic that separates "knows SQL" from "is good at SQL."

### ROW_NUMBER — assign a sequential number

```sql
-- Rank inspectors by number of inspections this quarter
SELECT
  inspector,
  COUNT(*) AS inspections,
  ROW_NUMBER() OVER (ORDER BY COUNT(*) DESC) AS rank
FROM qa_inspections
WHERE inspected_at >= DATE_TRUNC('quarter', CURRENT_DATE)
GROUP BY inspector;
```

### RANK vs DENSE_RANK

```sql
-- RANK: ties get the same rank, next rank is skipped
-- DENSE_RANK: ties get the same rank, next rank is NOT skipped
SELECT
  part_number,
  defect_count,
  RANK() OVER (ORDER BY defect_count DESC) AS rank,
  DENSE_RANK() OVER (ORDER BY defect_count DESC) AS dense_rank
FROM parts;

-- defect_count: 10, 10, 8, 7
-- RANK:         1,  1,  3, 4   (skips 2)
-- DENSE_RANK:   1,  1,  2, 3   (no skip)
```

### LAG / LEAD — access previous/next row

```sql
-- Show each inspection alongside the previous one for the same part
SELECT
  part_number,
  inspected_at,
  result,
  LAG(result) OVER (PARTITION BY part_number ORDER BY inspected_at) AS prev_result,
  LAG(inspected_at) OVER (PARTITION BY part_number ORDER BY inspected_at) AS prev_date
FROM qa_inspections
ORDER BY part_number, inspected_at;
```

### Running totals and moving averages

```sql
-- Running total of defects per day
SELECT
  DATE(inspected_at) AS day,
  COUNT(*) AS daily_defects,
  SUM(COUNT(*)) OVER (ORDER BY DATE(inspected_at)) AS cumulative_defects
FROM qa_inspections
WHERE result = 'fail'
GROUP BY DATE(inspected_at)
ORDER BY day;

-- 7-day moving average of defects
SELECT
  DATE(inspected_at) AS day,
  COUNT(*) AS daily_defects,
  ROUND(AVG(COUNT(*)) OVER (
    ORDER BY DATE(inspected_at)
    ROWS BETWEEN 6 PRECEDING AND CURRENT ROW
  ), 1) AS moving_avg_7d
FROM qa_inspections
WHERE result = 'fail'
GROUP BY DATE(inspected_at)
ORDER BY day;
```

### PARTITION BY — window within groups

```sql
-- For each operator, show their work orders ranked by completion time
SELECT
  op.name AS operator,
  wo.id,
  wo.completed_at - wo.started_at AS duration,
  RANK() OVER (
    PARTITION BY wo.operator_id
    ORDER BY wo.completed_at - wo.started_at
  ) AS speed_rank
FROM work_orders wo
JOIN operators op ON wo.operator_id = op.id
WHERE wo.status = 'completed';
```

---

## 2.6 CTEs (Common Table Expressions)

CTEs make complex queries readable. Use them instead of nested subqueries.

```sql
-- Find operators whose defect rate is above the facility average
WITH operator_stats AS (
  SELECT
    wo.operator_id,
    op.name,
    COUNT(*) AS total_orders,
    COUNT(*) FILTER (WHERE qi.result = 'fail') AS defect_orders
  FROM work_orders wo
  JOIN operators op ON wo.operator_id = op.id
  LEFT JOIN qa_inspections qi ON wo.id = qi.work_order_id
  GROUP BY wo.operator_id, op.name
),
facility_avg AS (
  SELECT AVG(defect_orders::float / NULLIF(total_orders, 0)) AS avg_defect_rate
  FROM operator_stats
)
SELECT
  os.name,
  os.total_orders,
  os.defect_orders,
  ROUND(100.0 * os.defect_orders / NULLIF(os.total_orders, 0), 1) AS defect_rate_pct,
  ROUND(100.0 * fa.avg_defect_rate, 1) AS facility_avg_pct
FROM operator_stats os
CROSS JOIN facility_avg fa
WHERE (os.defect_orders::float / NULLIF(os.total_orders, 0)) > fa.avg_defect_rate
ORDER BY defect_rate_pct DESC;
```

### Recursive CTE — hierarchical data

```sql
-- Bill of Materials: all sub-components of an assembly (tree traversal)
WITH RECURSIVE bom AS (
  -- Base case: top-level assembly
  SELECT id, part_number, parent_id, 1 AS depth
  FROM parts
  WHERE part_number = 'ASSY-100'

  UNION ALL

  -- Recursive case: children of current level
  SELECT p.id, p.part_number, p.parent_id, bom.depth + 1
  FROM parts p
  INNER JOIN bom ON p.parent_id = bom.id
)
SELECT depth, part_number
FROM bom
ORDER BY depth, part_number;
```

This is particularly relevant at a manufacturing facility — they deal with bill of materials (BOM) structures constantly.

---

## 2.7 Indexes and Performance

They may ask about query optimization.

```sql
-- Create an index for common query patterns
CREATE INDEX idx_qa_inspections_result_date
ON qa_inspections (result, inspected_at DESC);

-- Composite index for the most common lookup
CREATE INDEX idx_work_orders_status_operator
ON work_orders (status, operator_id)
WHERE deleted_at IS NULL; -- partial index, excludes soft-deleted rows
```

**EXPLAIN ANALYZE** — know how to read a query plan:
```sql
EXPLAIN ANALYZE
SELECT * FROM work_orders
WHERE status = 'in_progress' AND operator_id = 42;

-- Look for:
-- Seq Scan → table scan, no index (bad for large tables)
-- Index Scan → using an index (good)
-- Nested Loop → OK for small result sets
-- Hash Join → good for larger joins
-- Sort → look at whether it's in-memory or on-disk
```

---

## 2.8 Transactions and Locking

```sql
-- Atomic update: move inventory between locations
BEGIN;

UPDATE inventory
SET quantity = quantity - 10
WHERE part_id = 'PN-4421' AND location = 'warehouse-a';

UPDATE inventory
SET quantity = quantity + 10
WHERE part_id = 'PN-4421' AND location = 'production-floor';

COMMIT; -- both or neither
```

```sql
-- Advisory lock: prevent duplicate processing
SELECT pg_advisory_lock(hashtext('work-order-' || $1));

-- ... do work ...

SELECT pg_advisory_unlock(hashtext('work-order-' || $1));
```

You already use advisory locks in your deploy flow — mention this.

---

## 2.9 Practice Problems

Try writing these without looking at the answers:

1. **Find the top 5 parts by defect count in the last 90 days.**

2. **For each inspector, find their longest streak of consecutive passing inspections.**

3. **Write a query that returns the daily production output and the running 30-day average.**

4. **Given a `parts` table with `parent_id`, write a recursive CTE that returns the full assembly tree with indentation.**

5. **Find all operators who have never worked on a part that later failed QA.**

### Answers

**1.**
```sql
SELECT p.part_number, COUNT(*) AS defects
FROM qa_inspections qi
JOIN parts p ON qi.part_id = p.id
WHERE qi.result = 'fail'
  AND qi.inspected_at > NOW() - INTERVAL '90 days'
GROUP BY p.part_number
ORDER BY defects DESC
LIMIT 5;
```

**2.**
```sql
WITH numbered AS (
  SELECT inspector, result, inspected_at,
    ROW_NUMBER() OVER (PARTITION BY inspector ORDER BY inspected_at) AS rn,
    SUM(CASE WHEN result != 'pass' THEN 1 ELSE 0 END)
      OVER (PARTITION BY inspector ORDER BY inspected_at) AS fail_group
  FROM qa_inspections
),
streaks AS (
  SELECT inspector, rn - fail_group AS streak_id, COUNT(*) AS streak_len
  FROM numbered
  WHERE result = 'pass'
  GROUP BY inspector, streak_id
)
SELECT inspector, MAX(streak_len) AS longest_pass_streak
FROM streaks
GROUP BY inspector
ORDER BY longest_pass_streak DESC;
```

**3.**
```sql
SELECT
  DATE(completed_at) AS day,
  COUNT(*) AS daily_output,
  ROUND(AVG(COUNT(*)) OVER (
    ORDER BY DATE(completed_at)
    ROWS BETWEEN 29 PRECEDING AND CURRENT ROW
  ), 1) AS avg_30d
FROM work_orders
WHERE status = 'completed'
GROUP BY DATE(completed_at)
ORDER BY day;
```

**4.**
```sql
WITH RECURSIVE tree AS (
  SELECT id, part_number, parent_id, 0 AS depth,
    part_number::text AS path
  FROM parts WHERE parent_id IS NULL AND part_number = 'ASSY-100'
  UNION ALL
  SELECT p.id, p.part_number, p.parent_id, t.depth + 1,
    t.path || ' > ' || p.part_number
  FROM parts p
  JOIN tree t ON p.parent_id = t.id
)
SELECT REPEAT('  ', depth) || part_number AS indented, depth
FROM tree
ORDER BY path;
```

**5.**
```sql
SELECT DISTINCT op.name
FROM operators op
WHERE NOT EXISTS (
  SELECT 1
  FROM work_orders wo
  JOIN qa_inspections qi ON wo.part_id = qi.part_id
  WHERE wo.operator_id = op.id
    AND qi.result = 'fail'
    AND qi.inspected_at > wo.completed_at
);
```

---

# Part 3: Resume Alignment

## What to update on your resume before applying:

1. **Cloud section** — change "Vultr" to "Hivelocity" since you've migrated
2. **Remove "envelope encryption" bullet** — replaced by Vault, say "HashiCorp Vault for secrets management with GCP KMS auto-unseal"
3. **Add ITIL language** — in Clyde & Co description, add: "Participated in incident triage, change management, and post-implementation reviews for production deployments"
4. **Emphasize SQL** — add a line to Liquid Metal: "Wrote and optimized hand-tuned SQL queries (no ORM) for all data access patterns including advisory locking, transactional outbox, and batch operations"
5. **Reframe the Eggplant → Playwright migration** — your resume says "Playwright and the Chrome DevTools Protocol (CDP)" but doesn't mention Eggplant by name. Add: "Replaced legacy Eggplant (image-based/VNC) test suite with a custom Playwright for .NET framework" — Eggplant is on their job description, name-dropping it shows alignment
6. **Add HashiCorp stack** — your resume mentions Terraform and Nomad separately. Group them: "HashiCorp stack (Terraform, Nomad, Vault, Packer)" — Honeywell will notice the breadth

## What to bring up unprompted:

- Your transactional outbox pattern — this shows you understand data integrity at a level beyond "I can write CRUD"
- Your testing approach — 93 unit tests, integration tests against real Postgres. Say "I test against real databases, not mocks"
- Your documentation discipline — ARCHITECTURE.md, RUNBOOK.md, DEV.md. At a DOE facility, documentation is not optional
- Your infrastructure experience — Terraform, Nomad, bare metal. This shows you understand the full stack, not just application code

## What to connect to their world:

- **Blazor WebAssembly** → if they build internal web tools (likely), Blazor is a .NET frontend framework — mention you've shipped production Blazor apps. If their stack is Java-based, frame it as "I've built SPAs and server-rendered UIs — the framework is different, the patterns are the same."
- **Eggplant** → it's literally on their job description under "Experience in." You replaced it. That's a conversation starter, not a checkbox — "I've used Eggplant, identified its limitations for our environment, and migrated the team to Playwright."
- **Azure → their environment** → Clyde & Co used Azure (Function Apps, Blob Storage, Queues, SQL). Honeywell's internal tools likely have similar patterns — just different providers or on-prem equivalents. The concepts transfer: serverless compute, message queues, blob storage, relational databases.

## What NOT to bring up:

- Don't oversell the AI/ML work at Clyde & Co — this role is about traditional application development
- Don't talk about Kubernetes (they don't use it, and neither do you)
- Don't talk about scaling to millions of users — they care about reliability and correctness, not scale
- Don't compare Honeywell's tools unfavorably to modern alternatives — if they use Java or DOORS, respect it. They have reasons.

---

# Part 4: Behavioral Questions (STAR Format)

Every answer: **Situation** (context), **Task** (your responsibility), **Action** (what you did), **Result** (measurable outcome).

---

### "Tell me about yourself."

Keep it 90 seconds. Past → Present → Future.

"I studied software engineering at ASU, then spent the last three years at Clyde & Co as a developer and test automation engineer. On the dev side, I built Blazor WebAssembly frontends and .NET 8 APIs on Azure for a legal intake platform. On the automation side, I replaced a legacy image-based testing tool called Eggplant with a custom Playwright for .NET framework that I built from scratch — it automated browser testing against Windows Cloud PCs in a restricted enterprise network where Selenium couldn't connect. Outside of work, I've been building Liquid Metal, a deployment platform in Rust on bare metal servers I manage myself — raw SQL against Postgres, Terraform for infrastructure, GitHub Actions for CI/CD. I'm looking for a role where I can apply that full-stack discipline to systems that matter — and national security manufacturing is about as high-stakes as it gets."

---

### "Walk me through your resume."

Different from "tell me about yourself" — they want more detail. Go chronologically.

"I graduated from ASU with a BS in Software Engineering in 2022. Joined Clyde & Co in April 2023 as a Developer / Test Automation engineer.

My first big project was the legal intake platform — a cloud-native system that automated onboarding of global insurance clients. I built Blazor WebAssembly frontend components and .NET 8 REST APIs, with Azure SQL Database for persistence and Azure Function Apps for serverless compute. The platform captured about three million pounds in annual efficiency gains for the firm.

My second focus was test automation. The team was using Eggplant — an image-based tool that screen-scrapes via VNC. It was slow and brittle. I evaluated Selenium and Playwright, found that Selenium was blocked by our enterprise proxy, and built a custom Playwright for .NET framework that could tunnel through the proxy via CDP. Replaced Eggplant entirely in about four months.

I also partnered with the data science team on an AI model evaluation pipeline — replaced subjective manual review with a quantifiable 'Golden Response' strategy across 800+ curated prompts.

Outside of work, I've been building Liquid Metal — a mini PaaS in Rust. Five binaries, Firecracker VMs, Wasmtime, raw SQL, eBPF network isolation, the whole stack. It's deployed on bare metal I manage with Terraform and Nomad. That project is how I learned Rust, systems programming, and infrastructure management."

---

### "Tell me about a time you resolved a complex technical problem."

**Situation:** At Clyde & Co, the team was using Eggplant for test automation — an image-based tool that connects to machines via VNC and interacts with the UI by recognizing screenshots. It worked, but it was slow, brittle (a font change or theme update broke tests), and required its own proprietary scripting language (SenseTalk). When I joined, the team was doing most testing against Windows Cloud PCs in a restricted enterprise network. Eggplant's VNC approach worked through the network restrictions, but writing and maintaining tests was painful. Meanwhile, our .NET stack was growing and we had no modern test coverage.

**Task:** I needed to replace Eggplant with something that was faster, more maintainable, and could integrate into our .NET CI/CD pipeline — but it still had to work against Cloud PCs behind an enterprise proxy that blocked conventional browser automation tools.

**Action:** I evaluated Selenium first since it was the team's initial suggestion — industry standard, well-known. But Selenium's WebDriver protocol requires downloading browser drivers and opening ports that the enterprise proxy blocked. It couldn't connect to the Cloud PC environments at all.

I then looked at Playwright for .NET. Playwright bundles its own browser binaries (no external driver downloads), supports network interception and proxy configuration natively, and communicates via the Chrome DevTools Protocol (CDP) over WebSocket. The key insight was that Playwright's CDP connection could be configured to use the proxy's SOCKS5 tunnel rather than trying to bypass it. Playwright also has built-in auto-waiting (no more `Thread.Sleep` or explicit waits), browser context isolation (each test gets a clean "incognito" profile), and native .NET integration with MSTest/NUnit.

I built the framework on top of Playwright's .NET library using CDP connections. Wrote reusable page object abstractions so other team members could write tests using familiar .NET patterns without understanding the proxy workaround underneath. Established test authoring standards so new coverage could be added quickly.

**Result:** Replaced Eggplant entirely. Test execution time dropped dramatically — Playwright runs headless Chromium, not VNC screen-scraping. Tests were no longer brittle to UI theme changes because Playwright uses DOM selectors, not image recognition. The framework integrated directly into our Azure DevOps CI pipeline. Team members were writing new test coverage within a week because it was just C# and NUnit — no SenseTalk to learn.

---

### "Why Playwright over Selenium?"

If they drill into this (they might — it shows technical decision-making):

| Factor | Selenium | Playwright | Winner |
|--------|----------|------------|--------|
| Browser drivers | External download, version-matched | Bundled, managed automatically | Playwright |
| Waiting | Manual waits or explicit wait conditions | Auto-waiting built in | Playwright |
| Network interception | Not built in (needs proxy tool) | Native route interception, HAR recording | Playwright |
| Proxy support | Limited, driver-dependent | Native HTTP/SOCKS5 with auth and bypass rules | Playwright |
| Test isolation | Shared browser state (cleanup required) | Browser contexts — incognito-like, cheap to create | Playwright |
| .NET integration | Mature but verbose | First-class NUnit/MSTest base classes | Playwright |
| Codegen | Third-party tools | Built-in `playwright codegen` generates C# tests | Playwright |
| Trace viewer | Screenshots only | Full trace with DOM snapshots, network, console | Playwright |
| Enterprise networks | Blocked by proxy (WebDriver protocol) | CDP over configurable transport | Playwright |

"Selenium is a great tool, but for our specific environment — restricted enterprise network, .NET stack, Cloud PCs — Playwright solved problems Selenium couldn't. The proxy bypass, auto-waiting, and browser context isolation were the deciding factors."

---

### "Tell me about a time you worked with a business customer to deliver a solution."

**Situation:** At Clyde & Co, the data science team had a manual process for evaluating AI model upgrades — they'd review outputs by hand and make subjective judgment calls about whether a new model was better.

**Task:** Partner with data science to replace the manual review with something quantifiable and repeatable.

**Action:** Worked directly with the data scientists to understand their review criteria. Designed a "Golden Response" evaluation strategy — 800+ curated prompts across legal-domain categories with expected outputs. Built an automated regression pipeline that scored new model outputs against the golden set and surfaced regressions by category.

**Result:** Review cycles shortened significantly. The team could deploy model upgrades with measurable confidence instead of gut feeling. The pipeline caught edge-case regressions in legal terminology that manual review had missed.

---

### "Describe a time you had to learn a new technology quickly."

**Situation:** I decided to build Liquid Metal in Rust — a language I hadn't used professionally. The project required not just application code but systems-level work: Firecracker VM management, eBPF network classifiers, cgroup resource isolation, and async I/O.

**Task:** Get productive in Rust fast enough to build a working platform, not just toy programs.

**Action:** I learned by building. Started with the API layer (Axum — similar patterns to .NET web APIs I knew), then progressively took on harder pieces: async NATS consumers, ELF binary parsing, raw SQL with tokio-postgres, and eventually eBPF programs with Aya. I read the Rust book, studied open-source projects like Pingora and Firecracker, and wrote tests as I went to validate my understanding.

**Result:** Built a five-binary platform from scratch with 93 unit tests, integration tests, full CI/CD, and comprehensive documentation. The architecture doc alone is 600 lines. I went from zero Rust to writing eBPF TC classifiers in about a year.

---

### "How do you handle disagreements with team members?"

**Situation:** At Clyde & Co, the team was using Eggplant for test automation and there was initial resistance to replacing it. Eggplant was a known quantity — it worked through VNC, it didn't require code changes, and the QA team had years of SenseTalk scripts written. Some team members suggested Selenium as the replacement since it was "industry standard."

**Task:** I believed neither Eggplant (too brittle and slow) nor Selenium (blocked by our enterprise proxy) was the right choice, but I needed to make that case without just saying "I disagree."

**Action:** I set up proof-of-concepts for all three options and documented the results:
- **Eggplant**: showed the maintenance cost — a recent Windows theme update broke 30% of image-based tests, each requiring manual screenshot recapture
- **Selenium**: demonstrated that the WebDriver protocol was blocked by the enterprise proxy — couldn't connect to Cloud PCs at all
- **Playwright**: showed it connecting through the proxy via CDP, running tests headless in seconds instead of minutes, and generating readable C# test code that any .NET developer could maintain

Presented all three side by side with execution times, failure rates, and maintenance burden.

**Result:** The team agreed to go with Playwright based on the evidence. The QA team that had been writing SenseTalk was actually relieved — they preferred writing C# with NUnit to maintaining screenshot libraries. We decommissioned Eggplant within two months.

---

### "Tell me about a time you had to manage multiple priorities."

**Situation:** At Clyde & Co, I was maintaining the test automation framework, contributing to the legal intake platform, and partnering with data science on the evaluation pipeline — all simultaneously.

**Task:** Deliver on all three without dropping any.

**Action:** I prioritized by business impact. The intake platform had client deadlines, so that came first. The evaluation pipeline had a model release date, so that was second. Test automation was ongoing maintenance — I handled it in the gaps and automated what I could so it needed less attention. I communicated timelines honestly with each team rather than overcommitting.

**Result:** All three delivered on time. The intake platform shipped its integration. The evaluation pipeline was ready before the model release. The test framework kept running with minimal intervention.

---

### "Why Honeywell? Why this role?"

"Two reasons. First, I've spent the last three years building applications that support business operations — legal intake, model evaluation, test infrastructure. This role is the same thing in a higher-stakes environment. Supporting manufacturing operations for national security is meaningful work.

Second, I'm a full-stack developer who writes raw SQL, manages infrastructure, and cares about reliability and documentation. This role asks for someone who can work across the stack — from writing code to understanding business processes to doing code reviews. That's what I do every day."

---

### "What's your biggest weakness?"

Don't give a fake weakness. Give a real one with evidence you're managing it.

"I tend to go deep on problems. If I hit a bug, I want to understand the root cause, not just patch the symptom. That's usually a strength, but sometimes it means I spend more time on a problem than the business priority warrants. I've learned to set time limits — if I can't root-cause something in an hour, I document what I know, implement a workaround, and file a problem record for later."

---

# Part 5: COTS Tools They Use

You likely won't need deep expertise, but understanding what these tools do shows you researched the role.

### Enovia Matrix (Dassault Systemes)

**What it is:** Product Lifecycle Management (PLM) platform. Tracks the full lifecycle of a product from design through manufacturing to retirement.

**Why a DOE facility uses it:** They manufacture highly regulated components. Every design revision, every material change, every approval needs to be tracked and auditable. Enovia provides that traceability.

**What a developer does with it:** Customize workflows, build integrations with other systems (e.g., pull BOM data into the MES), write reports, configure access controls.

**If asked:** "I haven't worked with Enovia directly, but I understand PLM systems — they're the authoritative record for product configuration, revision history, and approval workflows. As a developer, I'd be working on customizations, integrations, and reports against the Enovia data model."

### IBM DOORS

**What it is:** Requirements management tool. Stores, tracks, and traces requirements throughout the development lifecycle.

**Why a DOE facility uses it:** When you're building national security components, every requirement must be traceable from specification to implementation to test. DOORS provides that bi-directional traceability.

**What a developer does with it:** Build integrations (DOORS ↔ other systems), automate traceability reports, write DXL scripts (DOORS eXtension Language — a proprietary scripting language), and build custom views/modules.

**DXL basics** (if they ask):
- DXL is a C-like scripting language embedded in DOORS
- Used for automating reports, bulk operations, and custom validations
- Similar to writing macros in Excel — automate repetitive tasks within the tool

**If asked:** "I know DOORS is the standard for requirements traceability in defense and nuclear programs. I haven't written DXL, but I've worked with domain-specific scripting languages before — the pattern is the same. I'd need to learn DXL syntax, but the logic of building traceability reports and automating workflows is familiar."

### PTC Windchill

**What it is:** PLM platform (competitor to Enovia). Manages product data, CAD documents, change processes, and BOMs. Built on a Java/Tomcat stack with a web-based UI.

**Why a DOE facility uses it:** Same reasons as Enovia — traceable product lifecycle, controlled change processes, auditable revision history. Government facilities often run multiple PLM tools because different programs or contractors brought different systems.

**What a developer does with it:** Windchill has a Java-based customization layer (Windchill customization toolkit). Developers write Java extensions, JSP/HTML views, custom reports, and integrations with other systems via REST APIs or Info*Engine (Windchill's integration engine). You might also write data migration scripts between Windchill and Enovia.

**If asked:** "I haven't worked with Windchill directly, but I understand the PLM domain — product configuration, BOM management, engineering change orders, revision control. Windchill's customization layer is Java-based, and while my primary languages are C# and Rust, I've done Java coursework at ASU and the patterns are the same — web APIs, database queries, business logic. I'd focus on learning the Windchill-specific APIs and data model."

### Solumina MES (iBase-t)

**What it is:** Manufacturing Execution System. Bridges the gap between PLM/ERP (what to build) and the shop floor (how it's being built). Tracks work orders, records inspection results, manages operator instructions, captures as-built data.

**Why a DOE facility uses it:** Every step of manufacturing must be documented and auditable. Solumina provides paperless work instructions, electronic records, and real-time production visibility.

**What a developer does with it:** Configure work flows, build integrations (MES ↔ PLM, MES ↔ ERP), write custom reports, develop shop-floor display applications.

**If asked:** "Solumina sits between the design system and the shop floor — it's where work orders become actual production steps with inspections and sign-offs. As a developer, I'd be building integrations, configuring workflows, and creating tools that help operators and inspectors do their jobs efficiently."

### SLSView

**What it is:** Less well-known. Likely an internal or niche tool for viewing/managing data in the context of stockpile lifecycle. Possibly related to the NNSA Stockpile Lifecycle Program.

**If asked:** "I'm not familiar with SLSView specifically. Can you tell me more about how the team uses it? I'm comfortable picking up new tools — I've gone from zero to productive in Rust, Terraform, and Nomad in the last year."

---

# Part 6: Security Clearance Prep

This role requires a DOE security clearance (likely Q clearance for NNSA work). Know what to expect.

### What they'll ask in the interview:
- "Are you a US citizen?" — Yes or no. Required, non-negotiable.
- "Are you willing to undergo a background investigation?" — Say yes.
- "Have you ever used illegal drugs?" — Be honest. Past marijuana use is usually not disqualifying if you're honest about it and it's not recent/ongoing.

### What the investigation covers:
- Employment history (10 years)
- Education verification
- Criminal record
- Financial history (credit check — they care about debt that could make you vulnerable to coercion)
- Foreign contacts and travel
- Drug use history
- Character references (they'll interview people you list AND people they find on their own)

### What to do NOW:
- Pull your credit report and make sure nothing is wrong
- Think about your references — who can speak to your character and reliability
- Don't lie on anything. Ever. Lying on a clearance application is a federal crime. Disqualifying information is often forgiven if you're upfront about it. Lying about it is never forgiven.

### Timeline:
- Interim clearance: weeks to months
- Full clearance: 6-12 months
- You can start work with an interim clearance in most cases

---

# Part 7: Mock Interview Questions

Based on the job description, here are the questions they're most likely to ask, with guidance on how to answer.

---

### Technical Questions

**Q: "Walk me through how you would design a new feature from requirements to deployment."**

"I start by understanding the business requirement — who needs it, why, and what does success look like. Then I check if there's an existing system or COTS tool that can be configured rather than building from scratch. If we're building, I design the solution — data model, integrations with existing systems, test strategy. I write the code with unit tests, get it code reviewed, and validate in a staging environment. Before production, I submit a change request with the risk assessment, rollback plan, and test results for approval. After deployment, I monitor for issues and verify the feature meets the original requirement. Then I document any lessons learned so the next change goes smoother."

*(This naturally covers the ITIL value chain — Engage, Design, Build, Change Enablement, Deliver, Continual Improvement — without name-dropping the framework.)*

**Q: "How do you approach debugging a production issue?"**

"First, I assess impact — how many users are affected and what's the urgency. That determines priority. Then I check logs and monitoring to narrow the scope. I look for what changed recently — was there a deployment, a config change, a data issue? Once I have a hypothesis, I verify it. If I can fix it quickly, I do. If not, I implement a workaround to restore service and then schedule the proper fix. After resolution, I document the root cause and whether we need a systemic fix to prevent recurrence."

**Q: "Tell me about your experience with SQL."**

"I write raw SQL by choice — no ORM. In my platform project, I use tokio-postgres with hand-written queries for everything: advisory locks to prevent duplicate deploys, a transactional outbox pattern that atomically writes to a services table and an outbox table in the same transaction, batch operations with FOR UPDATE SKIP LOCKED for concurrent processing, and 38 migrations managing the schema evolution. I'm comfortable with JOINs, window functions, CTEs including recursive CTEs, and query optimization with EXPLAIN ANALYZE."

**Q: "How do you handle code quality and testing?"**

"I write tests alongside the code, not after. I have 93 unit tests covering security-critical paths like SHA-256 verification, ELF binary parsing, feature flag parsing, and config validation. My integration tests run against a real Postgres database — I don't mock the database because mocks lie. I do code review, even on my own code — I re-read everything before committing. I use CI to run the full test suite on every push."

**Q: "What's your experience with CI/CD?"**

"At Clyde & Co, I maintained Azure DevOps pipelines. For my personal project, I built GitHub Actions workflows for CI (check + test on every push), release (cargo-dist builds cross-platform CLI binaries on git tag), and deployment (Nomad job updates via Tailscale SSH). The deployment pipeline includes automated testing gates — code doesn't reach production without passing tests."

---

### Behavioral/Culture Questions

**Q: "How do you stay current with technology?"**

"I build things. Liquid Metal taught me Rust, Firecracker, eBPF, Wasmtime, Terraform, and Nomad — all by building a real system, not by watching tutorials. I also read RFCs and source code when I need to understand something deeply. When I needed to parse ELF binaries for my platform, I read the ELF spec and wrote a parser from scratch rather than pulling in a library."

**Q: "How do you handle working in a classified/regulated environment?"**

"I already practice the disciplines that matter in a regulated environment. I document everything — architecture decisions, operational procedures, development guides. I follow change management processes even on my personal project — Terraform plan before apply, rolling deploys with health checks, rollback plans for every change. I write audit-ready code — every mutation is logged, every secret is encrypted, every access is authenticated."

**Q: "Describe your experience working with cross-functional teams."**

"At Clyde & Co, I worked across three teams simultaneously: legal operations (business stakeholders), data science (technical partners), and QA (testing). Each had different communication needs. Legal ops wanted business outcomes. Data science wanted technical accuracy. QA wanted testable deliverables. I adapted my communication to each audience and kept all three aligned through regular check-ins and shared documentation."

**Q: "What interests you about working at a DOE facility?"**

"The work matters. I've spent my career building tools that improve how people work — intake systems, evaluation pipelines, deployment platforms. At a DOE facility, that same work directly supports national security. The engineering discipline required — the documentation, the change control, the audit trails — matches how I already work. And honestly, I'm attracted to the stability and the mission. I want to build things that last."

---

### Questions YOU Should Ask

Ask 3-4 of these. Pick based on what feels natural in the conversation.

1. "What does a typical project look like for this role — is it mostly new feature development, integration work, or maintenance of existing systems?"
2. "Which of the COTS platforms (DOORS, Windchill, Solumina) does the team spend the most time working with?"
3. "How does the change management process work here — what does a typical RFC look like for a production deployment?"
4. "How large is the development team, and how are projects assigned?"
5. "What does the onboarding process look like, especially with the clearance timeline?"
6. "What's the tech stack for custom-built tools — is it mostly .NET, Java, Python, or a mix?"
7. "How does the team handle on-call or after-hours support?"
8. "What would success look like for someone in this role after 6 months?"

---

# Part 8: Day-of Checklist

- [ ] Print 3 copies of your resume (one for you, extras for interviewers)
- [ ] Bring a notebook and pen — take notes, it shows engagement
- [ ] Dress: business casual minimum. Government facility — err on the side of formal
- [ ] Arrive 15 minutes early. There may be a security gate.
- [ ] Bring government-issued photo ID (driver's license). They may check it at the gate.
- [ ] Phone on silent, not vibrate
- [ ] No smart watches or recording devices if they say so — classified facilities are strict about electronics
- [ ] Have your references ready (names, phone numbers, email) in case they ask
- [ ] Review the job description one more time in the parking lot
- [ ] Breathe. You built a platform from scratch in Rust. You can answer questions about SQL and change management.

---

# Part 9: Test Automation Deep Dive

This is a major part of your resume and they will ask about it. Know this cold.

---

## 9.1 Eggplant — What It Was and Why You Replaced It

**Eggplant** (now Keysight Eggplant) is an image-based test automation tool. Instead of interacting with the DOM like Selenium or Playwright, Eggplant:

1. Connects to the target machine via **VNC** (remote desktop protocol)
2. Takes screenshots of the screen
3. Uses **OCR and image recognition** to find UI elements (buttons, text fields, labels)
4. Sends mouse/keyboard events to interact with those elements
5. Scripts are written in **SenseTalk** — a proprietary, English-like scripting language

**Example SenseTalk script:**
```
Click "LoginButton"
WaitFor 10, "DashboardHeader"
TypeText "admin@company.com" into "EmailField"
Click "SubmitButton"
```

Where `"LoginButton"`, `"DashboardHeader"`, etc. are names of saved screenshot snippets that Eggplant matches against the live screen.

**Why enterprises use it:**
- Works on ANY platform — Windows, Mac, Linux, mainframes, Citrix, VDI
- No code changes needed to the application — purely external
- Non-developers can write tests (SenseTalk reads like English)
- VNC works through firewalls that block browser automation protocols

**Why you replaced it:**
- **Brittle**: Any UI change (font, theme, resolution, DPI) breaks image matches
- **Slow**: VNC screen-scraping + image recognition is orders of magnitude slower than DOM interaction
- **Maintenance burden**: Failed image matches require manually recapturing screenshots
- **No CI integration**: SenseTalk scripts don't integrate with .NET test runners or Azure DevOps pipelines
- **No developer adoption**: The dev team writes C# — learning SenseTalk for test automation was a hard sell
- **Licensing cost**: Enterprise licensing for Eggplant is expensive

## 9.2 Playwright for .NET — What You Built

**Playwright** is a browser automation library from Microsoft. Your framework used:

### Architecture
```
NUnit/MSTest test class
  → Page Object Model (reusable page abstractions)
    → Playwright .NET API
      → Chrome DevTools Protocol (CDP)
        → Bundled Chromium browser
          → Application under test (Cloud PC via proxy)
```

### Key Playwright Features You Used

**Auto-waiting:**
Playwright waits for elements to be visible, enabled, and stable before interacting. No `Thread.Sleep(5000)` or `WebDriverWait`.
```csharp
// Playwright auto-waits for the button to be clickable
await page.ClickAsync("#submit-button");

// vs Selenium — explicit wait required
var wait = new WebDriverWait(driver, TimeSpan.FromSeconds(10));
wait.Until(d => d.FindElement(By.Id("submit-button")).Displayed);
driver.FindElement(By.Id("submit-button")).Click();
```

**Browser context isolation:**
Each test gets a fresh browser context — like an incognito window. No shared cookies, local storage, or session state between tests.
```csharp
// Each test starts clean — no state leakage
await using var context = await browser.NewContextAsync();
var page = await context.NewPageAsync();
```

**Network interception:**
Intercept, modify, or mock network requests without external tools.
```csharp
// Mock an API response for testing
await page.RouteAsync("**/api/users", async route =>
{
    await route.FulfillAsync(new()
    {
        ContentType = "application/json",
        Body = "[{\"id\": 1, \"name\": \"Test User\"}]"
    });
});
```

**Proxy configuration:**
Native support for HTTP/SOCKS5 proxies — the key feature that made it work in your restricted network.
```csharp
var browser = await playwright.Chromium.LaunchAsync(new()
{
    Proxy = new()
    {
        Server = "socks5://proxy.corp.internal:1080",
        Bypass = "localhost,127.0.0.1"
    }
});
```

**Trace viewer:**
Records full execution traces with DOM snapshots, network requests, console logs, and screenshots at every step. Invaluable for debugging failed tests in CI.

### Your Framework's Reusable Abstractions

You built page object models and helper methods so the team could write tests without understanding the proxy/CDP internals:

```csharp
// What a test author writes (clean, readable)
[Test]
public async Task UserCanSubmitIntakeForm()
{
    var loginPage = new LoginPage(Page);
    await loginPage.LoginAs("test@company.com", "password");

    var intakePage = new IntakePage(Page);
    await intakePage.FillClientDetails("Acme Corp", "UK");
    await intakePage.SelectJurisdiction("England & Wales");
    await intakePage.Submit();

    await Expect(Page.Locator(".success-message"))
        .ToBeVisibleAsync();
}

// What the page object hides (proxy config, waits, selectors)
public class LoginPage
{
    private readonly IPage _page;

    public LoginPage(IPage page) => _page = page;

    public async Task LoginAs(string email, string password)
    {
        await _page.FillAsync("[data-testid='email']", email);
        await _page.FillAsync("[data-testid='password']", password);
        await _page.ClickAsync("[data-testid='login-submit']");
        await _page.WaitForURLAsync("**/dashboard");
    }
}
```

### Migration Timeline
```
Month 1: Evaluated Selenium (failed — proxy blocked), evaluated Playwright (worked)
Month 2: Built framework core, proxy bypass, first 20 tests
Month 3: Team onboarded, writing their own tests, Eggplant in parallel
Month 4: Eggplant decommissioned, all new tests in Playwright
```

## 9.3 Questions They Might Ask About This

**Q: "Why not just fix Eggplant instead of replacing it?"**

"The problem wasn't a bug in Eggplant — it was architectural. Image-based testing is fundamentally brittle for a web application that changes frequently. Every UI update required manual screenshot recapture. Playwright's DOM-based approach eliminates that entire category of maintenance. The effort to migrate was less than the ongoing cost of maintaining Eggplant scripts."

**Q: "How did you handle the team transition from Eggplant to Playwright?"**

"I made it easy. The framework abstracted away the complex parts (proxy configuration, CDP connections) behind page object models that were just regular C# classes. The team was already writing C# for the application — they didn't have to learn a new language. I wrote documentation, paired with team members on their first tests, and established conventions (data-testid attributes for selectors, page object pattern for all pages). Within a month, team members were writing tests independently."

**Q: "What was the hardest part of the Playwright implementation?"**

"Getting the CDP connection through the enterprise proxy. Playwright's default transport didn't work — the proxy was intercepting and blocking the WebSocket upgrade. I had to dig into Playwright's source to understand how it establishes the CDP connection, then configure it to tunnel through a SOCKS5 proxy endpoint that the security team had approved. Once that connection worked, everything else fell into place."

**Q: "How did you ensure test reliability?"**

"Three things. First, Playwright's auto-waiting eliminates the #1 source of flaky tests — timing issues. Second, browser context isolation means tests can't contaminate each other with leftover state. Third, I used `data-testid` attributes as selectors instead of CSS classes or XPath — they're stable identifiers that don't change when the UI is restyled. Our flake rate dropped from ~15% under Eggplant to under 2% with Playwright."
