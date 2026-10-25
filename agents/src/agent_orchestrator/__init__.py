"""Agent orchestration engine.

This package never holds provider API keys. Every model call is sent
to the Rust gateway with a short-lived, task-scoped access token.
"""


def service_name() -> str:
    return "agents"


def main() -> None:
    print(f"{service_name()} starting")
