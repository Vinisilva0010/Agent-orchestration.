# Security Policy

## Reporting a vulnerability

Please do not open a public issue for security problems.

Report privately through GitHub: open the **Security** tab of this repository and choose
**Report a vulnerability**. Include the affected component (gateway, server, sandbox, agents
or web), steps to reproduce and the impact you observed.

You will receive an acknowledgement as soon as the report is reviewed.

## Scope

Highest priority:

- Any way to read a provider API key or channel token.
- Any way to exceed a spending budget.
- Any way to run an irreversible action without human approval.
- Escaping the sandbox or reaching non-allowed folders or hosts.

## Supported versions

The project is in early development. Only the latest commit on `main` is supported.

See [THREAT_MODEL.md](THREAT_MODEL.md) for the design assumptions.
