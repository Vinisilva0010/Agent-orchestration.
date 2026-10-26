import httpx
import pytest
import respx

from agent_orchestrator.gateway import (
    BudgetExceededError,
    GatewayClient,
    GatewayError,
    ProviderUnavailableError,
    validate_base_url,
)

URL = "http://127.0.0.1:8081"
TOKEN = "tsk_test-token"


@pytest.mark.parametrize("url", ["https://gateway.example.com", "http://127.0.0.1:8081"])
def test_accepts_https_and_loopback_http(url: str) -> None:
    assert validate_base_url(url) == url


@pytest.mark.parametrize("url", ["http://gateway.example.com", "ftp://127.0.0.1", "127.0.0.1"])
def test_rejects_plain_http_off_loopback(url: str) -> None:
    with pytest.raises(ValueError, match="https"):
        validate_base_url(url)


def test_requires_a_task_token_and_hides_it() -> None:
    with pytest.raises(ValueError, match="tsk_"):
        GatewayClient(URL, "sk-not-a-task-token")
    assert TOKEN not in repr(GatewayClient(URL, TOKEN))


@respx.mock
def test_sends_only_the_task_token() -> None:
    route = respx.post(f"{URL}/v1/messages").mock(
        return_value=httpx.Response(200, json={"content": [], "stop_reason": "end_turn"})
    )
    GatewayClient(URL, TOKEN).complete({"tier": "small"})
    assert route.calls[0].request.headers["authorization"] == f"Bearer {TOKEN}"


@respx.mock
@pytest.mark.parametrize(
    ("status", "error_type"),
    [
        (402, BudgetExceededError),
        (429, ProviderUnavailableError),
        (503, ProviderUnavailableError),
        (400, GatewayError),
    ],
)
def test_maps_gateway_errors(status: int, error_type: type[GatewayError]) -> None:
    respx.post(f"{URL}/v1/messages").mock(
        return_value=httpx.Response(status, json={"error": {"code": "x", "message": "m"}})
    )
    with pytest.raises(error_type):
        GatewayClient(URL, TOKEN).complete({})


@respx.mock
def test_reports_an_unreachable_gateway() -> None:
    respx.post(f"{URL}/v1/messages").mock(side_effect=httpx.ConnectError("refused"))
    with pytest.raises(GatewayError, match="gateway_unreachable"):
        GatewayClient(URL, TOKEN).complete({})
