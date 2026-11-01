import { describe, expect, it } from "vitest";
import { approvalQueue, budgetQueue, pickRun } from "./runs";
import type { RunItem, WaitingRun } from "./types";

const item = (run_id: string | null): RunItem => ({
  run_id,
  task_start_id: run_id ? null : 1,
  status: run_id ? "running" : "queued",
  task: "t",
  requested_by: null,
  budget_micros: 1,
  cost_micros: 0,
  created_at: "2026-11-01T00:00:00Z",
  updated_at: "2026-11-01T00:00:00Z",
});

const approval = { call_id: "c", tool: "x", description: "", input: {}, irreversible: true, reason: "" };
const waiting = (id: string, overrides: Partial<WaitingRun> = {}): WaitingRun => ({
  id,
  agent_id: "a",
  agent_name: "A",
  status: "awaiting_approval",
  task: "t",
  budget_micros: 1,
  cost_micros: 0,
  approvals: [approval],
  can_decide: true,
  ...overrides,
});

describe("pickRun", () => {
  it("shows the chosen run while it is listed, else the newest run", () => {
    const items = [item(null), item("run_new"), item("run_old")];
    expect(pickRun(items, null)).toBe("run_new");
    expect(pickRun(items, "run_old")).toBe("run_old");
    expect(pickRun(items, "run_gone")).toBe("run_new");
    expect(pickRun([item(null)], null)).toBeNull();
    expect(pickRun([], "run_old")).toBeNull();
  });
});

describe("approvalQueue", () => {
  it("lists runs the person may decide first, keeping the server's order", () => {
    const queue = approvalQueue([
      waiting("run_view_only", { can_decide: false }),
      waiting("run_first"),
      waiting("run_budget", { status: "awaiting_budget", approvals: [] }),
      waiting("run_nothing_left", { approvals: [] }),
      waiting("run_second"),
    ]);
    expect(queue.map((run) => run.id)).toEqual(["run_first", "run_second", "run_view_only"]);
  });
});

describe("budgetQueue", () => {
  it("keeps only runs paused for budget", () => {
    expect(budgetQueue([waiting("a"), waiting("b", { status: "awaiting_budget" })]).map((run) => run.id)).toEqual(["b"]);
  });
});
