"""Client for the Rust gateway.

It holds only a short-lived task token, never provider keys. The token is sent
only to the configured gateway: HTTPS, or plain HTTP on loopback; redirects are
not followed and proxy settings from the environment are ignored.
"""

from typing import Any
from urllib.parse import urlsplit

import httpx

TASK_TOKEN_PREFIX = "tsk_"  # noqa: S105 (public prefix, not a secret)
# The gateway allows up to 600 s per provider call.
TIMEOUT_SECONDS = 620.0
LOOPBACK_HOSTS = frozenset({"127.0.0.1", "localhost", "::1"})
MAX_ERROR_CHARS = 300


class GatewayError(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(f"{status} {code}: {message}")
        self.status = status
        self.code = code
        self.message = message


class BudgetExceededError(GatewayError):
    """The task, agent or workspace budget is spent."""


class ProviderUnavailableError(GatewayError):
    """The model provider is overloaded, rate limited or unreachable."""


def validate_base_url(url: str) -> str:
    parts = urlsplit(url)
    if parts.scheme == "https" and parts.hostname:
        return url.rstrip("/")
    if parts.scheme == "http" and parts.hostname in LOOPBACK_HOSTS:
        return url.rstrip("/")
    raise ValueError("gateway URL must use https, or http on loopback only")


class GatewayClient:
    def __init__(self, base_url: str, task_token: str, http: httpx.Client | None = None) -> None:
        if not task_token.startswith(TASK_TOKEN_PREFIX):
            raise ValueError("task token must start with tsk_")
        self._base_url = validate_base_url(base_url)
        self._token = task_token
        self._http = http or httpx.Client(
            timeout=TIMEOUT_SECONDS, follow_redirects=False, trust_env=False
        )

    def __repr__(self) -> str:
        return f"GatewayClient(base_url={self._base_url!r}, token=<redacted>)"

    def complete(self, request: dict[str, Any]) -> dict[str, Any]:
        """Sends one neutral completion request and returns the gateway response."""
        try:
            response = self._http.post(
                f"{self._base_url}/v1/messages",
                json=request,
                headers={"authorization": f"Bearer {self._token}"},
            )
        except httpx.HTTPError as error:
            raise GatewayError(0, "gateway_unreachable", type(error).__name__) from None

        if response.status_code == httpx.codes.OK:
            body = response.json()
            if not isinstance(body, dict) or not isinstance(body.get("content"), list):
                raise GatewayError(502, "invalid_response", "unexpected gateway response")
            return body

        try:
            payload = response.json()
        except ValueError:
            payload = {}
        error = payload.get("error", {}) if isinstance(payload, dict) else {}
        if not isinstance(error, dict):
            error = {}
        code = str(error.get("code", "unknown"))
        message = str(error.get("message", ""))[:MAX_ERROR_CHARS]
        status = response.status_code
        if status == httpx.codes.PAYMENT_REQUIRED:
            raise BudgetExceededError(status, code, message)
        if status in (429, 502, 503):
            raise ProviderUnavailableError(status, code, message)
        raise GatewayError(status, code, message)
