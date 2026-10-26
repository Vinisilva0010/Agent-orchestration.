# Agent Orchestration

A self-hosted workspace where people and AI agents work together.

- Agents run on the company's own API key (Claude, OpenAI or Gemini).
- A token economy engine keeps spending to what each task actually needs.
- Hard spending limits per task, per agent and per month.
- The provider key never reaches the agents.
- Irreversible actions always require human approval.

## Status

Phase 2 in progress: Rust agent engine done, token economy next. Not ready for use.

## Repository layout

| Path       | Language | Purpose                                         |
|------------|----------|-------------------------------------------------|
| `gateway/` | Rust     | Provider gateway: holds the key, meters cost    |
| `server/`  | Rust     | Main server: auth, chat, realtime events, audit |
| `sandbox/` | Rust     | Isolated executor for code and file actions     |
| `engine/`  | Rust     | Agent engine: agent loop, approvals, durable runs |
| `protocol/` | Rust    | Wire format shared by engine and gateway      |
| `web/`     | TypeScript | Web interface (Next.js)                       |
| `installer/` | Shell  | One-command installer                           |
| `docs/`    | Markdown | Documentation site                              |
