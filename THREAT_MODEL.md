# Threat Model

Status: Phase 0 baseline. Reviewed at the end of every phase.
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

Rules:

- Provider keys and channel tokens exist only in the gateway/server secret store, encrypted at rest.
  They are never passed to the agents process, the browser, logs, or model prompts.
- The agents process is treated as potentially compromised (third-party Python dependencies).
  It can only reach the gateway and sandbox with short-lived, task-scoped, budget-limited tokens.
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

- The provider key and channel tokens never appear in any prompt, log line, error message or agents-process environment.
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
