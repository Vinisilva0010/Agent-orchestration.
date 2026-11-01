import type { RunItem, WaitingRun } from "./types";

/**
 * The run to show: the one the person chose while it is still listed,
 * otherwise the newest run. Queued requests have no run yet.
 */
export function pickRun(items: RunItem[], chosen: string | null): string | null {
  if (chosen && items.some((item) => item.run_id === chosen)) {
    return chosen;
  }
  return items.find((item) => item.run_id !== null)?.run_id ?? null;
}

/**
 * Runs waiting on a decision: the ones this person may decide first, then
 * the rest, each group in the server's order (longest waiting first).
 */
export function approvalQueue(runs: WaitingRun[]): WaitingRun[] {
  return runs
    .map((run, index) => ({ run, index }))
    .filter(({ run }) => run.status === "awaiting_approval" && run.approvals.length > 0)
    .sort((a, b) => Number(b.run.can_decide) - Number(a.run.can_decide) || a.index - b.index)
    .map(({ run }) => run);
}

/** Runs paused because their budget ran out. */
export function budgetQueue(runs: WaitingRun[]): WaitingRun[] {
  return runs.filter((run) => run.status === "awaiting_budget");
}
