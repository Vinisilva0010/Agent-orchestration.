"""Tools an agent may call, with validated arguments and an approval flag."""

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from pydantic import BaseModel, ValidationError

MAX_RESULT_CHARS = 20_000
MAX_REPORTED_PROBLEMS = 5


@dataclass(frozen=True)
class Tool:
    """A callable tool.

    `args` should forbid extra fields so unexpected input is rejected.
    `irreversible` tools run only after a person approves the exact call.
    """

    name: str
    description: str
    args: type[BaseModel]
    run: Callable[[Any], str]
    irreversible: bool = False

    def spec(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "description": self.description,
            "input_schema": self.args.model_json_schema(),
        }


class ToolRegistry:
    def __init__(self, tools: list[Tool]) -> None:
        names = [tool.name for tool in tools]
        if len(set(names)) != len(names):
            raise ValueError("tool names must be unique")
        self._tools = {tool.name: tool for tool in tools}

    def specs(self) -> list[dict[str, Any]]:
        return [tool.spec() for tool in self._tools.values()]

    def validate(self, name: str, raw_input: Any) -> tuple[Tool, BaseModel] | str:
        """Returns the tool and parsed arguments, or an error message for the model."""
        tool = self._tools.get(name)
        if tool is None:
            return f"unknown tool: {name[:100]}"
        if not isinstance(raw_input, dict):
            return "tool input must be a JSON object"
        try:
            return tool, tool.args.model_validate(raw_input)
        except ValidationError as error:
            details = "; ".join(
                f"{'.'.join(str(part) for part in problem['loc']) or 'input'}: {problem['msg']}"
                for problem in error.errors()[:MAX_REPORTED_PROBLEMS]
            )
            return f"invalid arguments for {name}: {details}"
