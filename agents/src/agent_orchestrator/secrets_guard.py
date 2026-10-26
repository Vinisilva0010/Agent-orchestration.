"""Refuses to run the agents process with secrets it must never hold.

The agents process runs third-party Python code and is treated as possibly
compromised. It may only hold the gateway URL and a short-lived task token.
Error messages name variables but never print their values.
"""

from collections.abc import Mapping

ALLOWED_NAMES = frozenset({"AGENT_GATEWAY_URL", "AGENT_TASK_TOKEN", "AGENT_TIER"})
FORBIDDEN_NAMES = frozenset(
    {"GATEWAY_ISSUER_SECRET", "GATEWAY_MASTER_KEY", "DATABASE_URL", "POSTGRES_PASSWORD"}
)
FORBIDDEN_NAME_PARTS = ("API_KEY", "SECRET", "PASSWORD", "PRIVATE_KEY", "ACCESS_KEY", "TOKEN")
# Value prefixes of provider keys (OpenAI, Anthropic, Google, xAI) and database URLs.
SECRET_VALUE_PREFIXES = ("sk-", "AIza", "AQ.", "xai-", "postgres://", "postgresql://")
# langchain-core can send conversation content to LangSmith when these are on.
TRACING_FLAGS = frozenset({"LANGSMITH_TRACING", "LANGCHAIN_TRACING_V2", "LANGCHAIN_TRACING"})
TRUE_VALUES = frozenset({"1", "true", "yes", "on"})


class UnsafeEnvironmentError(RuntimeError):
    """The process environment holds something the agents process must not have."""


def find_problems(env: Mapping[str, str]) -> list[str]:
    """Returns one line per unsafe variable, naming it without its value."""
    problems: list[str] = []
    for name, value in env.items():
        if name in ALLOWED_NAMES:
            continue
        upper = name.upper()
        stripped = value.strip()
        if upper in FORBIDDEN_NAMES or any(part in upper for part in FORBIDDEN_NAME_PARTS):
            problems.append(f"{name}: secret variables must not reach the agents process")
        elif stripped.startswith(SECRET_VALUE_PREFIXES):
            problems.append(f"{name}: value looks like a provider key or database URL")
        elif upper in TRACING_FLAGS and stripped.lower() in TRUE_VALUES:
            problems.append(f"{name}: tracing would send conversation content outside")
    return sorted(problems)


def check_environment(env: Mapping[str, str]) -> None:
    problems = find_problems(env)
    if problems:
        raise UnsafeEnvironmentError("unsafe environment: " + "; ".join(problems))
