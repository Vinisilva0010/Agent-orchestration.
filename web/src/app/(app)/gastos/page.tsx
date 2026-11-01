"use client";

import { AgentLimitRow } from "@/components/agent-limit-row";
import { BudgetCard } from "@/components/budget-card";
import { useMe } from "@/components/shell";
import { Card, ErrorNote, PageTitle, SpendLine } from "@/components/ui";
import { messageFor } from "@/lib/api";
import { formatMoney, formatPeriod } from "@/lib/format";
import { budgetQueue } from "@/lib/runs";
import type { AgentSummary, Usage, WaitingRun } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";

export default function SpendPage() {
  const me = useMe();
  const isAdmin = me?.role === "admin";
  const usage = usePoll<Usage>(isAdmin ? "/api/usage" : null);
  const waiting = usePoll<WaitingRun[]>("/api/runs");
  const agents = usePoll<AgentSummary[]>("/api/agents");
  const paused = budgetQueue(waiting.data ?? []);
  const listed = (agents.data ?? []).filter((agent) => !agent.archived || agent.month_spent_micros > 0);

  function refresh() {
    waiting.refresh();
    agents.refresh();
    usage.refresh();
  }

  return (
    <>
      <PageTitle subtitle="Quanto os agentes gastaram com modelos de IA neste mês.">Gastos</PageTitle>

      {isAdmin ? (
        <Card>
          {usage.data ? (
            <>
              <SpendLine title={`Total da empresa em ${formatPeriod(usage.data.period)}`}
                spent={usage.data.spent_micros} limit={usage.data.limit_micros} />
              {usage.data.reserved_micros > 0 ? (
                <p className="text-base">
                  Mais {formatMoney(usage.data.reserved_micros)} reservados para chamadas em andamento.
                </p>
              ) : null}
            </>
          ) : usage.error ? (
            <ErrorNote>{messageFor(usage.error)}</ErrorNote>
          ) : (
            <p className="text-lg">Carregando...</p>
          )}
        </Card>
      ) : null}

      <section className="flex flex-col gap-4" aria-labelledby="paused-title">
        <h2 id="paused-title" className="text-[28px] font-bold">Parado por orçamento</h2>
        {waiting.error ? <ErrorNote>{messageFor(waiting.error)}</ErrorNote> : null}
        {waiting.data && paused.length === 0 ? (
          <p className="text-lg">Nenhuma tarefa parada por falta de orçamento.</p>
        ) : null}
        <ul className="flex flex-col gap-6">
          {paused.map((run) => (
            <li key={run.id}><BudgetCard run={run} isAdmin={isAdmin} onChanged={refresh} /></li>
          ))}
        </ul>
      </section>

      <section className="flex flex-col gap-4" aria-labelledby="agents-spend-title">
        <h2 id="agents-spend-title" className="text-[28px] font-bold">Por agente</h2>
        {agents.error ? <ErrorNote>{messageFor(agents.error)}</ErrorNote> : null}
        {agents.data && listed.length === 0 ? <p className="text-lg">Nenhum agente para mostrar.</p> : null}
        {listed.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="w-full border-collapse">
              <caption className="sr-only">Gasto de cada agente neste mês</caption>
              <thead>
                <tr className="text-left text-base">
                  <th scope="col" className="pb-2 pr-4">Agente</th>
                  <th scope="col" className="pb-2 pr-4">Gasto no mês</th>
                  <th scope="col" className="pb-2 pr-4">Limite do mês</th>
                  {isAdmin ? <th scope="col" className="pb-2"><span className="sr-only">Ações</span></th> : null}
                </tr>
              </thead>
              <tbody>
                {listed.map((agent) => (
                  <AgentLimitRow key={agent.id} agent={agent} isAdmin={isAdmin} onChanged={refresh} />
                ))}
              </tbody>
            </table>
          </div>
        ) : null}
      </section>
    </>
  );
}
