# Threat Model

Status: Phase 2 in progress (Rust agent engine done; token economy next). Reviewed at the end of every phase.
Reference: OWASP Top 10 for Agentic Applications 2026 (ASI01–ASI10).

## 1. Assets

| Asset | Why it matters |
|---|---|
| Provider API keys (Anthropic, OpenAI, Google) | Theft enables unbounded spending on the customer's account |
| Channel credentials (Telegram bot token, future connectors) | Theft allows impersonating the workspace to its users |
| Company data reachable by agents (files, databases, messages) | Confidentiality and integrity of the customer's business |
| Spending budget | Uncontrolled agent loops can exhaust it within hours |
| Audit log | Required to explain and dispute any agent action |
| User accounts and sessions | Control over agents and approvals |

## 2. Trust boundaries
Browser / Telegram --(untrusted input)----> server + engine (Rust)
server + engine (Rust) --(task-scoped token)--> gateway (Rust) [only holder of provider keys]
gateway (Rust) --(HTTPS)--------------> model providers
server + engine (Rust) --(request)------------> sandbox (Rust) -> containers (tools)

Rules:

- Provider keys and channel tokens exist only in the gateway/server secret store, encrypted at rest.
  They are never passed to the agent engine, the browser, logs, or model prompts.
- The agent engine holds no provider keys and reaches the gateway only with short-lived,
  task-scoped, budget-limited tokens. Tools that need third-party code, including Python
  libraries, run in the sandbox, which is treated as potentially compromised.
- Everything an agent reads from outside (web pages, documents, emails, chat messages,
  tool output) is data, never instructions.

## 3. Threat actors

- External attacker sending content the agents will read (prompt injection).
- Malicious or compromised dependency (crates.io, PyPI, npm, GitHub Actions).
- Authenticated user exceeding their permissions.
- Outsider who obtained a Telegram account or a stolen session.
- The model itself making a harmful or costly decision.

## 4. Risks and mitigations (OWASP Agentic Top 10)

| ID | Risk | Planned mitigation | Phase |
|---|---|---|---|
| ASI01 | Agent goal hijack | External content tagged as untrusted data; system instructions separated from data; irreversible actions require human approval; injection attempts logged | 1, 3 |
| ASI02 | Tool misuse and exploitation | Per-agent tool allow-list; only tools for the current step are exposed; arguments validated by schema before execution | 1, 2 |
| ASI03 | Identity and privilege abuse | No long-lived credentials in agents; task-scoped tokens with expiry and budget; role-based permissions per user and per agent | 1, 3 |
| ASI04 | Agentic supply chain vulnerabilities | Lockfiles committed; cargo-deny, npm audit and uv hash checks in CI; actions pinned to commit SHAs; no third-party key-holding LLM proxy | 0 |
| ASI05 | Unexpected code execution | Code and file actions run only in isolated containers (gVisor when available) with allowed folders and network egress allow-list | 3 |
| ASI06 | Memory and context poisoning | Shared memory scoped per workspace and per agent; provenance stored with every memory entry; untrusted content never promoted to instructions | 2, 3 |
| ASI07 | Insecure inter-agent communication | Handoffs use a fixed, validated schema; each handoff re-issues scope instead of inheriting the sender's access | 2 |
| ASI08 | Cascading failures | Hard budgets per task, agent and month; loop and retry limits; circuit breaker stops a task and asks a human | 1, 2 |
| ASI09 | Human-agent trust exploitation | Approval cards show the exact action, target and cost; similar approvals grouped to avoid approval fatigue; admin decides which approvals Telegram may grant | 4, 5 |
| ASI10 | Rogue agents | Append-only audit log of every action, approval and cost; admin can pause or revoke any agent instantly | 3, 4 |

## 5. Security tests (must pass before a phase is closed)

- The provider key and channel tokens never appear in any prompt, log line, error message or agent engine environment.
- A task cannot spend past its budget, even with parallel calls.
- An expired or out-of-scope task token is rejected by the gateway.
- Prompt-injection suite: hidden instructions in documents and pages do not trigger tool calls.
- Sandbox: code cannot read outside allowed folders or reach non-allowed hosts.
- Telegram: messages from unlinked accounts are ignored and logged.

## 6. Accepted and open risks

- Full root compromise of the host can expose secrets in memory. Mitigation is limited to
  encryption at rest, least privilege for each service and documented hardening.
- Model providers process prompt content under their own terms; customers choose the provider.
- Gemini free-tier tests prove behavior, not production cost with other providers.
- The vault master key is read from an environment variable. Root on the host, or read
  access to the env file, exposes it. The installer (Phase 8) writes it with mode 600
  and owned by the gateway service user.
- If a provider call succeeds but recording its usage fails, the gateway logs an error
  for manual reconciliation and still returns the result. Phase 2 turns this into a
  persisted pending item shown to admins.
- OpenAI and Anthropic providers are tested only against a local fake server (no paid
  key). For OpenAI, three assumptions await a live call: input_tokens includes cached
  and written tokens, GPT-6 accepts reasoning effort `low`, and the GPT-6 reasoning
  item shape. Until then the long-prompt surcharge is applied above 272k tokens so
  spending is over-counted rather than under-counted.
- Free-tier Gemini keys let Google use prompts and responses to improve its products,
  with human review. Free tier is for development only; never send company data.
- Master key rotation is not implemented yet. Ciphertexts carry a key_version so a
  rotation command can re-encrypt them (Phase 8 or 9).
- Moving a run up a tier (overload, rate limits or repeated invalid tool
  calls) raises the cost per call. It is off by default
  (`AgentConfig::escalate_to = None`) and always bounded by the configured
  tier and by the task budget, since every call reserves its own price.
- Runs paused as `awaiting_budget` wait for a person with no expiry. The
  command to raise a task's limit arrives with the server (Phase 3); until
  then a paused run can only be resumed through the engine library.
- The `low_budget` flag compares the run's own spending with the remaining
  budget, so it is approximate when the task had spending before a resume.

## 7. Implemented controls (gateway and agent engine)

| Control | Where | Tested by |
|---|---|---|
| Provider keys encrypted with AES-256-GCM; master key never stored in the database | `gateway/src/vault.rs` | `vault` unit tests, live check for plaintext in the database |
| Fresh random nonce per write; provider and key version bound as associated data | `gateway/src/vault.rs` | `database_never_holds_the_plaintext`, `ciphertext_moved_to_another_provider_fails` |
| No route returns a provider key; credential routes require the issuer secret | `gateway/src/routes/credentials.rs` | `tests/credentials.rs` |
| Task tokens stored only as SHA-256 hashes, 15 min default and 1 h maximum lifetime, revocable | `gateway/src/tokens.rs` | `tests/tokens.rs` |
| Same response for missing, expired and revoked tokens; constant-time issuer check | `gateway/src/auth.rs` | `tests/tokens.rs` |
| Maximum possible cost reserved before each provider call, with row locks, so parallel calls cannot overspend | `gateway/src/budget.rs` | parallel reservation test (10 x 20 on a budget of 100) |
| Abandoned reservations released after 15 minutes | `gateway/src/budget.rs` | sweeper test |
| Append-only usage log enforced by database triggers | `gateway/migrations/` | `tests/db.rs` |
| Secrets wrapped in SecretString; headers never logged; Debug output redacted | `gateway/src/config.rs`, `gateway/src/lib.rs` | config tests, live log checks |
| Gateway binds to loopback by default; request bodies capped at 8 MiB | `gateway/src/config.rs`, `gateway/src/lib.rs` | config tests |
| Agent engine refuses to start with provider keys or the vault master key in its environment; errors name variables, never values | `engine/src/secrets_guard.rs` | unit tests, end-to-end run |
| Task token sent only to the gateway: HTTPS or loopback HTTP, no redirects or proxies, hidden from Debug output | `engine/src/gateway.rs` | `engine/tests/gateway_client.rs` |
| Irreversible tools pause for human approval; anything other than an explicit approval rejects; the decision is saved before anything runs | `engine/src/agent.rs`, `engine/src/runner.rs` | `engine/tests/agent.rs`, `engine/tests/store.rs` |
| Irreversible tools run at most once, across crashes, restarts and simultaneous approvals | `engine/src/store.rs` | `engine/tests/store.rs`, end-to-end run |
| Tool arguments parsed into Rust types with unknown fields rejected; invalid calls never reach approval or execution | `engine/src/tools.rs` | unit tests, `engine/tests/agent.rs` |
| Step limit per run and clean stop when the budget is spent | `engine/src/agent.rs` | `engine/tests/agent.rs` |
- A refused budget reservation (402) pauses the run as `awaiting_budget`
  before anything is spent; the run goes on only after a person raises the
  limit.
- After a tier change, reasoning blocks produced by the previous model are
  not sent to the new one; the saved history keeps them.
- Each run gets a fixed tool subset (`ToolRegistry::only`); tools outside it
  cannot be called even if the model asks for them, and the subset never
  changes mid-run, so the cached prefix stays byte-identical (tested).
- Each service connects with its own database role (`ao_gateway`, `ao_engine`)
  that has only the table operations its code runs. Neither role can create or
  change tables, read the other service's tables, or update or delete usage
  events; the engine role cannot read provider credentials, task tokens or
  budgets. Migrations and role passwords are applied only by `server migrate`
  with the owner's connection. A test checks every allowed and denied operation.
