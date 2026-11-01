"use client";

import Link from "next/link";
import { AgentCard } from "@/components/agent-card";
import { useMe } from "@/components/shell";
import { TaskForm } from "@/components/task-form";
import { Card, ErrorNote, IrreversibleTag, PageTitle } from "@/components/ui";
import { messageFor } from "@/lib/api";
import { STATUS_LABELS } from "@/lib/format";
import type { AgentSummary, WaitingRun } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";

export default function HomePage() {
  const me = useMe();
  const agents = usePoll<AgentSummary[]>("/api/agents");
  const waiting = usePoll<WaitingRun[]>("/api/runs");
  const usable = (agents.data ?? []).filter((agent) => agent.can_use);
  const visibleAgents = (agents.data ?? []).filter((agent) => !agent.archived);

  return (
    <>
      <PageTitle subtitle={me ? `Olá, ${me.display_name}.` : "Carregando..."}>Início</PageTitle>

      <section className="flex flex-col gap-4" aria-labelledby="waiting-title">
        <h2 id="waiting-title" className="text-[28px] font-bold">Esperando decisão</h2>
        {waiting.error ? <ErrorNote>{messageFor(waiting.error)}</ErrorNote> : null}
        {waiting.loading ? <p className="text-lg font-bold">Carregando...</p> : null}
        {waiting.data && waiting.data.length === 0 ? (
          <p className="text-lg font-bold">Nada esperando por você agora.</p>
        ) : null}
        <ul className="flex flex-col gap-3">
          {(waiting.data ?? []).map((run) => (
            <li key={run.id} className="flex flex-col gap-2 rounded-2xl border-2 border-warn-ink bg-warn-bg p-5 text-warn-ink">
              <div className="flex flex-wrap items-center gap-3">
                <span className="text-lg font-bold">{run.agent_name ?? run.agent_id}</span>
                <span className="text-base font-bold">{STATUS_LABELS[run.status]}</span>
                {run.approvals.some((approval) => approval.irreversible) ? <IrreversibleTag /> : null}
              </div>
              <p className="line-clamp-3 whitespace-pre-wrap break-words text-lg text-ink">{run.task}</p>
              <Link href={run.status === "awaiting_approval" ? `/aprovacoes#${run.id}` : `/agentes/${encodeURIComponent(run.agent_id)}`}
                className="self-start text-lg font-bold text-ink underline">
                {run.status === "awaiting_approval" ? "Ver e decidir" : "Ver a tarefa"}
              </Link>
            </li>
          ))}
        </ul>
      </section>

      {usable.length > 0 ? (
        <section className="flex flex-col gap-4" aria-labelledby="start-title">
          <h2 id="start-title" className="text-[28px] font-bold">Começar uma tarefa</h2>
          <Card>
            <TaskForm agents={usable} onSent={agents.refresh} />
          </Card>
        </section>
      ) : null}

      <section className="flex flex-col gap-4" aria-labelledby="agents-title">
        <h2 id="agents-title" className="text-[28px] font-bold">Agentes</h2>
        {agents.error ? <ErrorNote>{messageFor(agents.error)}</ErrorNote> : null}
        {agents.data && visibleAgents.length === 0 ? (
          <p className="text-lg font-bold">Nenhum agente disponível para você. Peça acesso a um administrador.</p>
        ) : null}
        <ul className="grid gap-4 md:grid-cols-2">
          {visibleAgents.map((agent) => <AgentCard key={agent.id} agent={agent} />)}
        </ul>
      </section>
    </>
  );
}
