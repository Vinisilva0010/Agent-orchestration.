/** Shapes returned by the Rust server. Money is in millionths of a dollar. */

export type AgentSummary = {
  id: string;
  display_name: string;
  description: string;
  tier: "small" | "standard" | "large";
  task_budget_micros: number;
  archived: boolean;
  /** Tasks waiting for a person: an approval or more budget. */
  waiting: number;
  running: number;
  month_spent_micros: number;
  month_limit_micros: number | null;
  can_use: boolean;
  can_manage: boolean;
};

export type RunStatus =
  | "queued"
  | "running"
  | "awaiting_approval"
  | "awaiting_budget"
  | "completed"
  | "budget_exceeded"
  | "max_steps"
  | "failed";

export type RunItem = {
  run_id: string | null;
  task_start_id: number | null;
  status: RunStatus;
  task: string;
  requested_by: string | null;
  budget_micros: number;
  cost_micros: number;
  created_at: string;
  updated_at: string;
};

export type Approval = {
  call_id: string;
  tool: string;
  /** Written by the tool's developers. */
  description: string;
  input: unknown;
  irreversible: boolean;
  /** Written by the model: untrusted, shown as text. */
  reason: string;
};

export type Entry = { role: "person" | "agent"; text: string; tools: string[] };

export type ActivityOutcome = "done" | "error" | "rejected" | "waiting" | "started" | "not_run";

export type Activity = {
  call_id: string;
  tool: string;
  outcome: ActivityOutcome;
  result: string | null;
  irreversible: boolean;
  started_at: string | null;
  finished_at: string | null;
};

export type RunDetail = {
  id: string;
  agent_id: string;
  agent_name: string | null;
  status: RunStatus;
  task: string;
  requested_by: string | null;
  budget_micros: number;
  cost_micros: number;
  created_at: string;
  updated_at: string;
  final_text: string;
  error: string | null;
  conversation: Entry[];
  activity: Activity[];
  approvals: Approval[];
  can_decide: boolean;
  can_cancel: boolean;
  can_raise_budget: boolean;
};

export type WaitingRun = {
  id: string;
  agent_id: string;
  agent_name: string | null;
  status: "awaiting_approval" | "awaiting_budget";
  task: string;
  budget_micros: number;
  cost_micros: number | null;
  approvals: Approval[];
  can_decide: boolean;
};
