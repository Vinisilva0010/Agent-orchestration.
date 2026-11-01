"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { useState } from "react";
import { RunView } from "@/components/run-view";
import { TaskForm } from "@/components/task-form";
import { Badge, Card, ErrorNote, PageTitle, SpendLine } from "@/components/ui";
import { ApiError, messageFor } from "@/lib/api";
import { STATUS_LABELS, agentStatus, formatMoney, formatTime, statusTone } from "@/lib/format";
import { pickRun } from "@/lib/runs";
import type { AgentSummary, RunDetail, RunItem } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";

export default function AgentPage() {
  const { id } = useParams<{ id: string }>();
  const agentPath = `/api/agents/${encodeURIComponent(id)}`;
  const agent = usePoll<AgentSummary>(agentPath);
  const runs = usePoll<RunItem[]>(`${agentPath}/runs`);
  const [chosen, setChosen] = useState<string | null>(null);
  const selected = pickRun(runs.data ?? [], chosen);
  const run = usePoll<RunDetail>(selected ? `/api/runs/${encodeURIComponent(selected)}` : null);

  function refreshAll() {
    agent.refresh();
    runs.refresh();
    run.refresh();
  }

  if (agent.error instanceof ApiError && agent.error.status === 404) {
    return (
      <>
        <PageTitle>Agente não encontrado</PageTitle>
        <p className="text-lg font-bold">Este agente não existe ou você não tem acesso a ele.</p>
        <Link href="/agentes" className="text-lg font-bold underline">Voltar para Agentes</Link>
      </>
    );
  }
  if (!agent.data) {
    return agent.error ? <ErrorNote>{messageFor(agent.error)}</ErrorNote> : <p className="text-lg font-bold">Carregando...</p>;
  }

  const status = agentStatus(agent.data);
  return (
    <>
      <div className="flex flex-col gap-3">
        <PageTitle>{agent.data.display_name}</PageTitle>
        <div><Badge tone={status.tone}>{status.label}</Badge></div>
        {agent.data.description ? (
          <p className="whitespace-pre-wrap break-words text-xl">{agent.data.description}</p>
        ) : null}
      </div>

      <Card>
        <SpendLine title="Gasto deste mês" spent={agent.data.month_spent_micros} limit={agent.data.month_limit_micros} />
      </Card>

      {agent.data.can_use ? (
        <section className="flex flex-col gap-4" aria-labelledby="new-task-title">
          <h2 id="new-task-title" className="text-[28px] font-bold">Nova tarefa</h2>
          <Card>
            <TaskForm agents={[agent.data]} onSent={refreshAll} />
          </Card>
        </section>
      ) : null}

      <section className="flex flex-col gap-4" aria-labelledby="tasks-title">
        <h2 id="tasks-title" className="text-[28px] font-bold">Tarefas</h2>
        {runs.error ? <ErrorNote>{messageFor(runs.error)}</ErrorNote> : null}
        {runs.data && runs.data.length === 0 ? (
          <p className="text-lg font-bold">Este agente ainda não recebeu tarefas.</p>
        ) : null}
        <ul className="flex flex-col gap-2">
          {(runs.data ?? []).map((item) => {
            const key = item.run_id ?? `queued-${item.task_start_id}`;
            const current = item.run_id !== null && item.run_id === selected;
            return (
              <li key={key}>
                <button type="button" disabled={!item.run_id} aria-pressed={current}
                  onClick={() => item.run_id && setChosen(item.run_id)}
                  className={`flex w-full flex-col gap-1 rounded-[10px] border-2 px-4 py-3 text-left ${current ? "border-ink bg-ink text-paper" : "border-ink bg-paper text-ink"}`}>
                  <span className="flex flex-wrap items-center gap-3">
                    <Badge tone={statusTone(item.status)}>{STATUS_LABELS[item.status]}</Badge>
                    <span className="text-base font-bold">
                      {formatTime(item.created_at)}
                      {item.requested_by ? ` · ${item.requested_by}` : ""}
                      {` · ${formatMoney(item.cost_micros)} de ${formatMoney(item.budget_micros)}`}
                    </span>
                  </span>
                  <span className="line-clamp-2 whitespace-pre-wrap break-words text-lg font-bold">{item.task}</span>
                </button>
              </li>
            );
          })}
        </ul>
      </section>

      {run.data ? (
        <section className="flex flex-col gap-4" aria-labelledby="run-title">
          <h2 id="run-title" className="text-[28px] font-bold">Tarefa escolhida</h2>
          <RunView run={run.data} onChanged={refreshAll} />
        </section>
      ) : run.error ? (
        <ErrorNote>{messageFor(run.error)}</ErrorNote>
      ) : null}
    </>
  );
}
