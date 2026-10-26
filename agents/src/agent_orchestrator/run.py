"""Command line runner: python -m agent_orchestrator.run "task".

Reads AGENT_GATEWAY_URL and AGENT_TASK_TOKEN. Prints one JSON event per line.
When an irreversible action needs approval, reads one line from stdin:
"approve" runs it, anything else rejects it.

Exit codes: 0 completed, 2 budget exceeded, 3 other failure, 4 unsafe
environment or configuration, 64 usage error.
"""

import json
import os
import sys
import uuid
from datetime import UTC, datetime
from typing import Any

from pydantic import BaseModel, ConfigDict

from agent_orchestrator.agent import AgentConfig, build_graph, pending_approval, resume, start
from agent_orchestrator.gateway import GatewayClient
from agent_orchestrator.secrets_guard import UnsafeEnvironmentError, check_environment
from agent_orchestrator.tools import Tool, ToolRegistry

DEFAULT_GATEWAY_URL = "http://127.0.0.1:8081"
EXIT_CODES = {"completed": 0, "budget_exceeded": 2}


class NoArgs(BaseModel):
    model_config = ConfigDict(extra="forbid")


class ArchiveArgs(BaseModel):
    model_config = ConfigDict(extra="forbid")
    report_id: str = "latest"


def demo_tools() -> ToolRegistry:
    def get_time(_: NoArgs) -> str:
        return datetime.now(UTC).isoformat(timespec="seconds")

    def archive_report(args: ArchiveArgs) -> str:
        return f"report {args.report_id} archived (demo tool: nothing was changed)"

    return ToolRegistry(
        [
            Tool("get_time", "Returns the current UTC time.", NoArgs, get_time),
            Tool(
                "archive_report",
                "Archives a report. This cannot be undone.",
                ArchiveArgs,
                archive_report,
                irreversible=True,
            ),
        ]
    )


def emit(event: dict[str, Any]) -> None:
    print(json.dumps(event, ensure_ascii=False), flush=True)


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        print('usage: python -m agent_orchestrator.run "task"', file=sys.stderr)
        return 64
    try:
        check_environment(os.environ)
        client = GatewayClient(
            os.environ.get("AGENT_GATEWAY_URL", DEFAULT_GATEWAY_URL),
            os.environ.get("AGENT_TASK_TOKEN", ""),
        )
    except (UnsafeEnvironmentError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 4
    os.environ["LANGSMITH_TRACING"] = "false"

    graph = build_graph(
        client, demo_tools(), AgentConfig(tier=os.environ.get("AGENT_TIER", "standard"))
    )
    thread_id = str(uuid.uuid4())
    result = start(graph, thread_id, args[0])
    while (approval := pending_approval(result)) is not None:
        emit({"event": "approval_required", **approval})
        answer = sys.stdin.readline().strip().lower()
        decision = "approve" if answer == "approve" else "reject"
        emit({"event": "decision", "decision": decision})
        result = resume(graph, thread_id, decision)

    status = result.get("status", "error")
    emit(
        {
            "event": "finished",
            "status": status,
            "steps": result.get("steps", 0),
            "final_text": result.get("final_text", ""),
            "error": result.get("error"),
        }
    )
    return EXIT_CODES.get(status, 3)


if __name__ == "__main__":
    raise SystemExit(main())
