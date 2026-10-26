import json
import uuid
from typing import Any

import httpx
import pytest
import respx
from pydantic import BaseModel, ConfigDict

from agent_orchestrator.agent import (
    REJECTED_RESULT,
    AgentConfig,
    build_graph,
    pending_approval,
    resume,
    start,
)
from agent_orchestrator.gateway import GatewayClient
from agent_orchestrator.tools import Tool, ToolRegistry

URL = "http://127.0.0.1:8081"
MESSAGES = f"{URL}/v1/messages"
TOKEN = "tsk_test-token"


class NoArgs(BaseModel):
    model_config = ConfigDict(extra="forbid")


class ArchiveArgs(BaseModel):
    model_config = ConfigDict(extra="forbid")
    report_id: str


def reply(content: list[dict[str, Any]], stop_reason: str) -> httpx.Response:
    return httpx.Response(
        200,
        json={
            "provider": "mock",
            "model": "mock-standard",
            "content": content,
            "stop_reason": stop_reason,
            "usage": {},
            "cost_micros": 1,
            "task_budget_remaining_micros": 100,
        },
    )


def text(value: str) -> dict[str, Any]:
    return {"type": "text", "text": value}


def tool_use(name: str, tool_input: Any, call_id: str = "call_1", **extra: Any) -> dict:
    return {"type": "tool_use", "id": call_id, "name": name, "input": tool_input, **extra}


def sent(route: respx.Route, index: int) -> dict[str, Any]:
    return json.loads(route.calls[index].request.content)


@pytest.fixture
def counts() -> dict[str, int]:
    return {"get_time": 0, "archive_report": 0}


@pytest.fixture
def registry(counts: dict[str, int]) -> ToolRegistry:
    def get_time(_: NoArgs) -> str:
        counts["get_time"] += 1
        return "2026-10-26T12:00:00+00:00"

    def archive(args: ArchiveArgs) -> str:
        counts["archive_report"] += 1
        return f"archived {args.report_id}"

    return ToolRegistry(
        [
            Tool("get_time", "Current time.", NoArgs, get_time),
            Tool("archive_report", "Archives a report.", ArchiveArgs, archive, irreversible=True),
        ]
    )


def make_graph(registry: ToolRegistry, **settings: Any) -> Any:
    return build_graph(GatewayClient(URL, TOKEN), registry, AgentConfig(**settings))


def thread() -> str:
    return str(uuid.uuid4())


@respx.mock
def test_answers_text_and_sends_cached_system_and_tools(registry: ToolRegistry) -> None:
    route = respx.post(MESSAGES).mock(side_effect=[reply([text("done")], "end_turn")])
    result = start(make_graph(registry, tier="small"), thread(), "hello")
    assert result["status"] == "completed"
    assert result["final_text"] == "done"
    request = sent(route, 0)
    assert request["tier"] == "small"
    assert request["system"][0]["cache"] is True
    assert {tool["name"] for tool in request["tools"]} == {"get_time", "archive_report"}


@respx.mock
def test_reversible_tool_runs_and_model_output_is_sent_back_unchanged(
    registry: ToolRegistry, counts: dict[str, int]
) -> None:
    first = [
        {"type": "reasoning", "text": "", "signature": "sig-1"},
        tool_use("get_time", {}, signature="gemini-sig"),
    ]
    route = respx.post(MESSAGES).mock(
        side_effect=[reply(first, "tool_use"), reply([text("it is noon")], "end_turn")]
    )
    result = start(make_graph(registry), thread(), "what time is it?")
    assert result["status"] == "completed"
    assert counts["get_time"] == 1
    second = sent(route, 1)
    assert second["messages"][1] == {"role": "assistant", "content": first}
    assert second["messages"][2]["content"][0]["tool_use_id"] == "call_1"
    assert second["messages"][2]["content"][0]["is_error"] is False


@respx.mock
def test_irreversible_tool_waits_for_approval(
    registry: ToolRegistry, counts: dict[str, int]
) -> None:
    route = respx.post(MESSAGES).mock(
        side_effect=[
            reply([tool_use("archive_report", {"report_id": "q3"})], "tool_use"),
            reply([text("archived")], "end_turn"),
        ]
    )
    graph = make_graph(registry)
    thread_id = thread()
    paused = start(graph, thread_id, "archive q3")
    approval = pending_approval(paused)
    assert approval == {
        "kind": "approval",
        "actions": [{"id": "call_1", "tool": "archive_report", "input": {"report_id": "q3"}}],
    }
    assert counts["archive_report"] == 0
    assert route.call_count == 1

    result = resume(graph, thread_id, "approve")
    assert result["status"] == "completed"
    assert counts["archive_report"] == 1


@respx.mock
@pytest.mark.parametrize("decision", ["reject", "", "yes please", {"other": "approve"}])
def test_anything_but_approval_rejects(
    registry: ToolRegistry, counts: dict[str, int], decision: Any
) -> None:
    route = respx.post(MESSAGES).mock(
        side_effect=[
            reply([tool_use("archive_report", {"report_id": "q3"})], "tool_use"),
            reply([text("not archived")], "end_turn"),
        ]
    )
    graph = make_graph(registry)
    thread_id = thread()
    start(graph, thread_id, "archive q3")
    result = resume(graph, thread_id, decision)
    assert result["status"] == "completed"
    assert counts["archive_report"] == 0
    tool_message = sent(route, 1)["messages"][2]["content"][0]
    assert tool_message["is_error"] is True
    assert tool_message["content"] == REJECTED_RESULT


@respx.mock
def test_invalid_arguments_skip_approval_and_go_back_to_the_model(
    registry: ToolRegistry, counts: dict[str, int]
) -> None:
    route = respx.post(MESSAGES).mock(
        side_effect=[
            reply([tool_use("archive_report", {"_unparsed_arguments": "{"})], "tool_use"),
            reply([text("sorry")], "end_turn"),
        ]
    )
    result = start(make_graph(registry), thread(), "archive")
    assert pending_approval(result) is None
    assert result["status"] == "completed"
    assert counts["archive_report"] == 0
    tool_message = sent(route, 1)["messages"][2]["content"][0]
    assert tool_message["is_error"] is True
    assert "invalid arguments" in tool_message["content"]


@respx.mock
def test_stops_when_the_budget_is_spent(registry: ToolRegistry) -> None:
    respx.post(MESSAGES).mock(
        return_value=httpx.Response(
            402, json={"error": {"code": "budget_exceeded", "message": "task budget exceeded"}}
        )
    )
    result = start(make_graph(registry), thread(), "hello")
    assert result["status"] == "budget_exceeded"
    assert "budget" in result["error"]


@respx.mock
def test_stops_at_the_step_limit(registry: ToolRegistry, counts: dict[str, int]) -> None:
    route = respx.post(MESSAGES).mock(
        side_effect=lambda _request: reply([tool_use("get_time", {})], "tool_use")
    )
    result = start(make_graph(registry, max_steps=2), thread(), "loop")
    assert result["status"] == "max_steps"
    assert route.call_count == 2
    assert counts["get_time"] == 2
