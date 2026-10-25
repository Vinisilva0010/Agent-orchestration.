from agent_orchestrator import service_name


def test_reports_service_name() -> None:
    assert service_name() == "agents"
