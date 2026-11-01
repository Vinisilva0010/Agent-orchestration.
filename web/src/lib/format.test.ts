import { describe, expect, it } from "vitest";
import {
  agentStatus,
  checkNewTaskBudget,
  formatPeriod,
  countDecisions,
  formatMoney,
  formatTime,
  isFinished,
  parseMoney,
  percentUsed,
  statusTone,
  taskLength,
  toolDescription,
  toolLabel,
} from "./format";
import type { WaitingRun } from "./types";

/** Intl puts a no-break space after the currency symbol. */
const plain = (text: string) => text.replace(/ /g, " ");

describe("formatMoney", () => {
  it("writes dollars the Brazilian way", () => {
    expect(plain(formatMoney(3_120_000))).toBe("US$ 3,12");
    expect(plain(formatMoney(0))).toBe("US$ 0,00");
    expect(plain(formatMoney(1_234_560_000))).toBe("US$ 1.234,56");
    expect(plain(formatMoney(100_000_000))).toBe("US$ 100,00");
  });

  it("never shows a small spend as zero", () => {
    expect(plain(formatMoney(1))).toBe("menos de US$ 0,01");
    expect(plain(formatMoney(9_999))).toBe("menos de US$ 0,01");
    expect(plain(formatMoney(10_000))).toBe("US$ 0,01");
  });

  it("keeps the no-break space so the symbol never wraps alone", () => {
    expect(formatMoney(3_120_000)).toBe("US$ 3,12");
  });
});

describe("parseMoney", () => {
  it("reads amounts written in common ways", () => {
    expect(parseMoney("3,12")).toBe(3_120_000);
    expect(parseMoney("3.12")).toBe(3_120_000);
    expect(parseMoney("10")).toBe(10_000_000);
    expect(parseMoney(" US$ 10,5 ")).toBe(10_500_000);
    expect(parseMoney("1.234,56")).toBe(1_234_560_000);
    expect(parseMoney("1.000")).toBe(1_000_000_000);
    expect(parseMoney("0,01")).toBe(10_000);
  });

  it("refuses anything that is not a positive amount in cents", () => {
    for (const bad of ["", "abc", "-5", "0", "0,00", "0,001", "1,2,3", "3,125", "1e3", "10000000000", "1.23.4", "1,234.56", "0.123"]) {
      expect(parseMoney(bad), bad).toBeNull();
    }
  });
});

describe("percentUsed", () => {
  it("stays between 0 and 100", () => {
    expect(percentUsed(50, 100)).toBe(50);
    expect(percentUsed(150, 100)).toBe(100);
    expect(percentUsed(-1, 100)).toBe(0);
    expect(percentUsed(10, null)).toBe(0);
    expect(percentUsed(10, 0)).toBe(0);
  });
});

describe("formatTime", () => {
  it("formats server times and ignores bad input", () => {
    expect(formatTime("2026-11-01T14:03:00Z")).toMatch(/^\d{2}\/\d{2}\/2026, \d{2}:\d{2}$/);
    expect(formatTime(null)).toBe("");
    expect(formatTime("not a date")).toBe("");
  });
});

describe("statuses", () => {
  it("groups run statuses by what a person must do", () => {
    expect(statusTone("awaiting_approval")).toBe("waiting");
    expect(statusTone("awaiting_budget")).toBe("waiting");
    expect(statusTone("queued")).toBe("working");
    expect(statusTone("completed")).toBe("done");
    expect(statusTone("failed")).toBe("stopped");
    expect(isFinished("running")).toBe(false);
    expect(isFinished("budget_exceeded")).toBe(true);
  });

  it("gives an agent one status, waiting first", () => {
    expect(agentStatus({ archived: true, waiting: 2, running: 1 }).label).toBe("Arquivado");
    expect(agentStatus({ archived: false, waiting: 1, running: 1 })).toEqual({
      label: "1 tarefa espera decisão",
      tone: "waiting",
    });
    expect(agentStatus({ archived: false, waiting: 3, running: 0 }).label).toBe("3 tarefas esperam decisão");
    expect(agentStatus({ archived: false, waiting: 0, running: 2 }).label).toBe("Trabalhando");
    expect(agentStatus({ archived: false, waiting: 0, running: 0 }).label).toBe("Sem tarefas agora");
  });
});

describe("countDecisions", () => {
  const run = (overrides: Partial<WaitingRun>): WaitingRun => ({
    id: "run_1",
    agent_id: "a",
    agent_name: "A",
    status: "awaiting_approval",
    task: "t",
    budget_micros: 1,
    cost_micros: 0,
    approvals: [],
    can_decide: true,
    can_cancel: true,
    ...overrides,
  });

  it("counts actions the person may decide", () => {
    const approval = { call_id: "c", tool: "x", description: "", input: {}, irreversible: true, reason: "" };
    expect(
      countDecisions([
        run({ approvals: [approval, { ...approval, call_id: "d" }] }),
        run({ can_decide: false, approvals: [approval] }),
        run({ status: "awaiting_budget" }),
        run({}),
      ]),
    ).toBe(3);
  });
});

describe("text helpers", () => {
  it("counts task characters as the server does", () => {
    expect(taskLength("  ação  ")).toBe(4);
    expect(taskLength("😀")).toBe(1);
  });

  it("names known tools in plain words", () => {
    expect(toolLabel("archive_report")).toBe("Arquivar um relatório");
    expect(toolLabel("custom_tool")).toBe("custom_tool");
    expect(toolDescription("archive_report", "Archives a report.")).toBe(
      "Arquiva um relatório. Isto não pode ser desfeito.",
    );
    expect(toolDescription("custom_tool", "Does things.")).toBe("Does things.");
  });
});

describe("formatPeriod", () => {
  it("names the month in Portuguese", () => {
    expect(formatPeriod("2026-11")).toBe("novembro de 2026");
    expect(formatPeriod("2026-03")).toBe("março de 2026");
    expect(formatPeriod("2026-13")).toBe("2026-13");
  });
});

describe("checkNewTaskBudget", () => {
  it("accepts only a higher amount within the task maximum", () => {
    expect(checkNewTaskBudget("5,00", 2_000_000)).toEqual({ micros: 5_000_000 });
    expect(checkNewTaskBudget("2", 2_000_000)).toHaveProperty("error");
    expect(checkNewTaskBudget("abc", 2_000_000)).toHaveProperty("error");
    expect(checkNewTaskBudget("100,01", 2_000_000)).toHaveProperty("error");
    expect(checkNewTaskBudget("100", 2_000_000)).toEqual({ micros: 100_000_000 });
  });
});
