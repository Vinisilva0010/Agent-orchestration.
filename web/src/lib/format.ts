/**
 * Plain-language text for what the server returns: money, times, statuses.
 * Pure functions, so they are tested without a browser.
 */
import type { ActivityOutcome, AgentSummary, RunStatus, WaitingRun } from "./types";

const MICROS_PER_DOLLAR = 1_000_000;
/** One cent, in millionths of a dollar. */
const CENT_MICROS = 10_000;

const money = new Intl.NumberFormat("pt-BR", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

/** "US$ 3,12". Amounts above zero but below a cent read "menos de US$ 0,01". */
export function formatMoney(micros: number): string {
  if (micros > 0 && micros < CENT_MICROS) {
    return `menos de ${money.format(0.01)}`;
  }
  return money.format(micros / MICROS_PER_DOLLAR);
}

/**
 * Reads an amount typed by a person ("3,12", "3.12", "1.234,56", "US$ 10")
 * The comma is always the decimal mark, so "3,125" is refused, never read as
 * three thousand.
 * as millionths of a dollar. Returns null when it is not a positive amount
 * with at most two decimals.
 */
export function parseMoney(text: string): number | null {
  const value = text.trim().replace(/^US\$\s*/i, "");
  let whole: string;
  let fraction = "";
  let match: RegExpMatchArray | null;
  if ((match = value.match(/^([1-9]\d{0,2}(?:\.\d{3})+)(?:,(\d{1,2}))?$/))) {
    // 1.234,56 (a comma is always the decimal mark here)
    whole = match[1].replace(/\./g, "");
    fraction = match[2] ?? "";
  } else if ((match = value.match(/^(\d+)(?:[.,](\d{1,2}))?$/))) {
    whole = match[1];
    fraction = match[2] ?? "";
  } else {
    return null;
  }
  if (whole.length > 9) {
    return null;
  }
  const micros = Number(whole) * MICROS_PER_DOLLAR + Number(fraction.padEnd(2, "0")) * CENT_MICROS;
  return Number.isSafeInteger(micros) && micros > 0 ? micros : null;
}

/** Share of a limit used, from 0 to 100, for progress bars. */
export function percentUsed(spent: number, limit: number | null): number {
  if (!limit || limit <= 0) {
    return 0;
  }
  return Math.min(100, Math.max(0, Math.round((spent / limit) * 100)));
}

const time = new Intl.DateTimeFormat("pt-BR", {
  day: "2-digit",
  month: "2-digit",
  year: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

/** "01/11/2026, 14:03" in the person's own time zone. */
export function formatTime(iso: string | null): string {
  if (!iso) {
    return "";
  }
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? "" : time.format(date);
}

export const STATUS_LABELS: Record<RunStatus, string> = {
  queued: "Na fila para começar",
  running: "Trabalhando",
  awaiting_approval: "Esperando decisão",
  awaiting_budget: "Parado por orçamento",
  completed: "Concluída",
  budget_exceeded: "Orçamento esgotado",
  max_steps: "Parou no limite de passos",
  failed: "Não concluída",
};

export type Tone = "neutral" | "working" | "waiting" | "stopped" | "done";

export function statusTone(status: RunStatus): Tone {
  switch (status) {
    case "queued":
    case "running":
      return "working";
    case "awaiting_approval":
    case "awaiting_budget":
      return "waiting";
    case "completed":
      return "done";
    default:
      return "stopped";
  }
}

export function isFinished(status: RunStatus): boolean {
  return !["queued", "running", "awaiting_approval", "awaiting_budget"].includes(status);
}

/** One status for an agent, from its counts. */
export function agentStatus(agent: Pick<AgentSummary, "archived" | "waiting" | "running">): {
  label: string;
  tone: Tone;
} {
  if (agent.archived) {
    return { label: "Arquivado", tone: "neutral" };
  }
  if (agent.waiting > 0) {
    return {
      label: agent.waiting === 1 ? "1 tarefa espera decisão" : `${agent.waiting} tarefas esperam decisão`,
      tone: "waiting",
    };
  }
  if (agent.running > 0) {
    return { label: "Trabalhando", tone: "working" };
  }
  return { label: "Sem tarefas agora", tone: "neutral" };
}

export const OUTCOME_LABELS: Record<ActivityOutcome, string> = {
  done: "Feito",
  error: "Deu erro",
  rejected: "Recusado por uma pessoa",
  waiting: "Esperando",
  started: "Começou, mas não se sabe se terminou",
  not_run: "Não foi feito",
};

const TOOL_LABELS: Record<string, string> = {
  get_time: "Ver a hora",
  archive_report: "Arquivar um relatório",
  run_python: "Rodar um programa em Python",
};

/**
 * What known tools do, in Portuguese. Like the engine's own descriptions
 * (which are in English, for the model), these are written by developers,
 * never by the model.
 */
const TOOL_DESCRIPTIONS: Record<string, string> = {
  get_time: "Consulta a hora atual. Não muda nada.",
  archive_report: "Arquiva um relatório. Isto não pode ser desfeito.",
  run_python: "Roda um programa em Python num ambiente isolado, sem internet.",
};

/** The description to show: Portuguese for known tools, else the engine's. */
export function toolDescription(tool: string, engineDescription: string): string {
  return TOOL_DESCRIPTIONS[tool] ?? engineDescription;
}

/** A name people understand; unknown tools keep their own name. */
export function toolLabel(tool: string): string {
  return TOOL_LABELS[tool] ?? tool;
}

/** Approvals that wait on someone who may decide them, counted for the menu. */
export function countDecisions(runs: WaitingRun[]): number {
  return runs
    .filter((run) => run.can_decide && run.status === "awaiting_approval")
    .reduce((total, run) => total + Math.max(1, run.approvals.length), 0);
}

/** Input of a tool call as readable JSON, for the technical details. */
export function prettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? "";
  } catch {
    return String(value);
  }
}

export const TASK_MAX_CHARS = 8000;

/** Counts characters as the server does (Unicode code points). */
export function taskLength(text: string): number {
  return Array.from(text.trim()).length;
}

const MONTHS = [
  "janeiro", "fevereiro", "março", "abril", "maio", "junho",
  "julho", "agosto", "setembro", "outubro", "novembro", "dezembro",
];

/** "2026-11" as "novembro de 2026". */
export function formatPeriod(period: string): string {
  const match = /^(\d{4})-(\d{2})$/.exec(period);
  const month = match ? Number(match[2]) : 0;
  return match && month >= 1 && month <= 12 ? `${MONTHS[month - 1]} de ${match[1]}` : period;
}

export const LEVEL_LABELS = { use: "Pode usar", manage: "Pode usar e gerenciar" } as const;

/**
 * Checks a new task budget typed by an administrator: a valid amount, above
 * the current budget, and at most US$ 100 (the server's limit for a task).
 */
export function checkNewTaskBudget(text: string, currentMicros: number): { micros: number } | { error: string } {
  const micros = parseMoney(text);
  if (micros === null) {
    return { error: "Digite um valor em dólares, por exemplo 5,00." };
  }
  if (micros <= currentMicros) {
    return { error: `O novo limite precisa ser maior que ${formatMoney(currentMicros)}.` };
  }
  if (micros > 100_000_000) {
    return { error: `O limite de uma tarefa é no máximo ${formatMoney(100_000_000)}.` };
  }
  return { micros };
}

export const PASSWORD_MIN = 15;
export const PASSWORD_MAX = 128;
