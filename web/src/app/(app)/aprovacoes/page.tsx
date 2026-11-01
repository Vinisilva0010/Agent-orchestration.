"use client";

import { ApprovalCard } from "@/components/approval-card";
import { ErrorNote, PageTitle } from "@/components/ui";
import { messageFor } from "@/lib/api";
import { approvalQueue } from "@/lib/runs";
import type { WaitingRun } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";

export default function ApprovalsPage() {
  const waiting = usePoll<WaitingRun[]>("/api/runs");
  const queue = approvalQueue(waiting.data ?? []);

  return (
    <>
      <PageTitle subtitle="Ações que os agentes só fazem depois que uma pessoa aprova.">Aprovações</PageTitle>
      {waiting.error ? <ErrorNote>{messageFor(waiting.error)}</ErrorNote> : null}
      {waiting.loading ? <p className="text-lg font-bold">Carregando...</p> : null}
      {waiting.data && queue.length === 0 ? (
        <p className="text-lg font-bold">Nenhuma ação esperando aprovação.</p>
      ) : null}
      <ul className="flex flex-col gap-6">
        {queue.map((run) => (
          <li key={run.id} id={run.id}>
            <ApprovalCard runId={run.id} agentName={run.agent_name ?? run.agent_id} task={run.task}
              approvals={run.approvals} canDecide={run.can_decide} onDecided={waiting.refresh} />
          </li>
        ))}
      </ul>
    </>
  );
}
