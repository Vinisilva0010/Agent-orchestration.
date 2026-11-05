# Agent Orchestration

A self-hosted workspace where people and AI agents work together, with hard
limits on what agents can see, spend and do.

Agents run tasks on frontier models through your own API keys. Every
irreversible action waits for a person's approval, every model call reserves
its maximum cost before it starts, and every change is written to an
append-only, hash-chained audit log.

> **Status:** in active development. Phases 1 to 4 are complete (gateway,
> engine, sandbox, security hardening and the web interface). Not ready for
> production use yet.

## What it guarantees

- **Provider keys never leak.** Keys live encrypted in the gateway's vault.
  Agents, the engine, the sandbox and the web server never receive a provider
  key or the master key, and refuse to start if one is in their environment.
- **Spend never passes a limit.** Limits exist per workspace (monthly), per
  agent (monthly) and per task. Each model call reserves its maximum cost
  first; a task that runs out of budget stops and waits for an administrator.
- **People decide irreversible actions.** Tools marked irreversible stop the
  task until someone approves or rejects them. "What will happen" is written
  by developers in the tool definition, never by the model.
- **Everything is audited.** Database triggers write every change (accounts,
  sessions, permissions, keys, budgets, runs, approvals) to a hash-chained,
  append-only log, with the person who made it.
- **Least privilege everywhere.** Each service connects to PostgreSQL with
  its own role and only the grants its code uses; tests check those grants.

See [THREAT_MODEL.md](THREAT_MODEL.md) for the full model and
[ATTACKS.md](ATTACKS.md) for the attacks the test suite proves are blocked.

## How it fits together

| Part | What it does |
|------|--------------|
| `gateway` | Vault for provider keys, budgets and usage, model calls (Anthropic, OpenAI, Gemini and a mock provider for tests). |
| `server` | Accounts, sessions, roles, per-agent permissions and the HTTP API the web interface uses. Also applies all database migrations. |
| `engine` | Runs agents: model calls through the gateway, tools, approvals, budget stops. `engine worker` processes requests made in the web interface. |
| `sandbox` | Runs Python in a locked-down container: no network, read-only, no capabilities, memory, CPU, process and time limits. |
| `protocol` | Types shared by the services. |
| `bench` | Measurement tools. |
| `web` | The interface: home, agents, approvals, spending, people and settings. |

Default local ports: gateway `8081`, server `8090`, sandbox `8095`,
web `3000`, PostgreSQL `5433`. Everything binds to `127.0.0.1`.

## Web interface

Light, high-contrast and plain Brazilian Portuguese, built for people who are
not technical:

- **Início**: what is waiting for a decision, start a task, the agents.
- **Agentes**: each agent's tasks, conversation, what it did, results and spend.
- **Aprovações**: irreversible actions explained before anyone approves.
- **Gastos**: this month against the limit, tasks stopped by budget, limits per agent.
- **Pessoas** and **Configurações** (administrators): accounts, roles,
  permissions and provider keys (typed once, never shown back).

The pages run under a strict Content Security Policy with a per-request nonce;
sessions are `__Host-` cookies (Secure, HttpOnly, SameSite=Strict) checked by
the Rust server on every request.

## Requirements

- Linux (developed on Ubuntu under WSL2)
- Rust (stable) and Cargo
- Docker with Compose v2
- Node.js and npm (for `web/`; see `web/package.json`)

## Try it with the mock provider

The fastest way to see everything working, at no cost: this starts the
gateway with its mock provider, the server, the engine worker and the web
interface on a throwaway database (`ao_e2e`), separate from your development
database.

```bash
docker compose -f compose.dev.yaml up -d --wait
scripts/e2e-stack.sh start
```

Open http://localhost:3100 and sign in with `admin@e2e.test` /
`e2e administrator passphrase`. The mock model does not understand text; end a
task with `[mock:tool:archive_report]` to see an approval request, or with
`[mock:tool:get_time]` to see a tool run. Stop everything with:

```bash
scripts/e2e-stack.sh stop
```

`scripts/e2e-stack.sh` is also the reference for how each service is started
and which variables each one receives.

## Development setup

1. Copy `.env.example` to `.env` and fill in every value. Never commit `.env`.
   Provider keys do **not** go in `.env`: an administrator stores them in the
   gateway's vault from **Configurações**.
2. Start PostgreSQL and build:

   ```bash
   docker compose -f compose.dev.yaml up -d --wait
   cargo build --workspace
   ```

3. Apply the migrations and create the first administrator (the password is
   read from standard input; 15 to 128 characters):

   ```bash
   (set -a && . ./.env && set +a && ./target/debug/server migrate)
   (set -a && . ./.env && set +a && ./target/debug/server create-admin you@example.com "Your Name")
   ```

   `create-admin` only works while the workspace has no administrator.

4. Start the services (see `scripts/e2e-stack.sh` for the exact variables of
   each), then the interface:

   ```bash
   cd web && npm ci && npm run dev
   ```

   In development, Next.js forwards `/api/*` to the server
   (`SERVER_API_URL`, default `http://127.0.0.1:8090`), so start the server
   with `SERVER_PUBLIC_ORIGIN=http://localhost:3000`.

### Command-line tools

| Command | Purpose |
|---------|---------|
| `server migrate` | Apply all migrations and set the service roles' passwords. |
| `server create-admin EMAIL NAME` | Create the first administrator (password on stdin). |
| `server serve` | Run the HTTP API. |
| `engine worker [--once]` | Process task, approval, budget and cancel requests. |
| `engine run "TASK"` | Run one task from the terminal. |
| `engine decide RUN_ID approve\|reject` | Answer a waiting approval from the terminal. |
| `sandbox prepare` | Pull the pinned Python image. |
| `sandbox serve` | Run the sandbox service for the engine's `run_python` tool. |

### Server settings

| Variable | Meaning |
|----------|---------|
| `SERVER_PUBLIC_ORIGIN` | Where the interface is served, e.g. `https://agents.example.com`. Requests from other origins are refused. |
| `SERVER_BIND` | Listen address (default `127.0.0.1:8090`). |
| `SERVER_TRUSTED_PROXIES` | Reverse proxy IPs whose `X-Forwarded-For` is believed for sign-in rate limits (default: none). |

## Tests

```bash
# Rust: unit, integration and database tests (PostgreSQL must be running)
(set -a && . ./.env && set +a && cargo test --workspace --locked)

# Every named attack in ATTACKS.md must have a passing test
scripts/attack-suite.sh

# Web interface
cd web && npm run lint && npm test && npm run build

# Browser tests of the whole workspace (mock provider, throwaway database,
# Playwright inside its official pinned image; nothing installed on the host)
scripts/e2e.sh
```

GitHub Actions runs formatting, Clippy, the Rust tests, the attack suite,
`cargo deny`, and the web lint, tests, audit and build on every push.

## Project layout
gateway/ key vault, budgets, model providers
server/ accounts, permissions, HTTP API, migrations
engine/ agent runs, approvals, worker
sandbox/ isolated Python execution
protocol/ shared types
bench/ measurement tools
web/ Next.js interface and browser tests (web/e2e)
scripts/ attack suite and end-to-end helpers
docs/ working notes

## Roadmap

- [x] Phases 1 to 3: gateway, engine and sandbox, with the attack suite
- [x] Phase 4: web interface
- [ ] Phase 5: more model providers, including any OpenAI-compatible API
      (DeepSeek, Qwen, Kimi, GLM, MiniMax, OpenRouter), with per-model prices
- [ ] Phase 6: agent builder: instructions, model, tools, approvals and
      permissions per agent
- [ ] Phase 7: chat with agents in the web interface and on Telegram
- [ ] Phase 8: connectors and an egress allowlist
- [ ] Phase 9: installer, reverse proxy and a gVisor sandbox runtime
- [ ] Phase 10: external anchoring of the audit log and recovery of stalled runs

## License

No license has been chosen yet. All rights reserved.
