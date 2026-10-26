import pytest

from agent_orchestrator.secrets_guard import (
    UnsafeEnvironmentError,
    check_environment,
    find_problems,
)


def test_accepts_only_agent_settings() -> None:
    check_environment(
        {
            "PATH": "/usr/bin",
            "HOME": "/home/user",
            "AGENT_GATEWAY_URL": "http://127.0.0.1:8081",
            "AGENT_TASK_TOKEN": "tsk_abc",
            "LANGSMITH_TRACING": "false",
        }
    )


@pytest.mark.parametrize(
    "name",
    [
        "GEMINI_API_KEY",
        "OPENAI_API_KEY",
        "GATEWAY_ISSUER_SECRET",
        "GATEWAY_MASTER_KEY",
        "DATABASE_URL",
        "POSTGRES_PASSWORD",
        "GITHUB_TOKEN",
    ],
)
def test_rejects_secret_variable_names(name: str) -> None:
    with pytest.raises(UnsafeEnvironmentError, match=name):
        check_environment({name: "anything"})


@pytest.mark.parametrize(
    "value",
    ["sk-proj-abc", "sk-ant-abc", "AIzaSyabc", "AQ.Ab8abc", "postgres://u:p@h/db"],
)
def test_rejects_values_that_look_like_keys(value: str) -> None:
    problems = find_problems({"HARMLESS_NAME": value})
    assert len(problems) == 1
    assert value not in problems[0], "values must never be printed"


@pytest.mark.parametrize("name", ["LANGSMITH_TRACING", "LANGCHAIN_TRACING_V2"])
def test_rejects_external_tracing(name: str) -> None:
    with pytest.raises(UnsafeEnvironmentError, match="tracing"):
        check_environment({name: "true"})


def test_error_message_never_contains_values() -> None:
    with pytest.raises(UnsafeEnvironmentError) as error:
        check_environment({"GEMINI_API_KEY": "AQ.real-looking-secret"})
    assert "real-looking-secret" not in str(error.value)
