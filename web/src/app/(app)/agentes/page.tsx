"use client";

import { AgentCard } from "@/components/agent-card";
import { ErrorNote, PageTitle } from "@/components/ui";
import { messageFor } from "@/lib/api";
import type { AgentSummary } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";

export default function AgentsPage() {
  const agents = usePoll<AgentSummary[]>("/api/agents");
  const active = (agents.data ?? []).filter((agent) => !agent.archived);
  const archived = (agents.data ?? []).filter((agent) => agent.archived);

  return (
    <>
      <PageTitle subtitle="Os agentes que você pode ver.">Agentes</PageTitle>
      {agents.error ? <ErrorNote>{messageFor(agents.error)}</ErrorNote> : null}
      {agents.loading ? <p className="text-lg font-bold">Carregando...</p> : null}
      {agents.data && active.length === 0 ? (
        <p className="text-lg font-bold">Nenhum agente disponível para você. Peça acesso a um administrador.</p>
      ) : null}
      <ul className="grid gap-4 md:grid-cols-2">
        {active.map((agent) => <AgentCard key={agent.id} agent={agent} />)}
      </ul>
      {archived.length > 0 ? (
        <section className="flex flex-col gap-4" aria-labelledby="archived-title">
          <h2 id="archived-title" className="text-[28px] font-bold">Arquivados</h2>
          <ul className="grid gap-4 md:grid-cols-2">
            {archived.map((agent) => <AgentCard key={agent.id} agent={agent} />)}
          </ul>
        </section>
      ) : null}
    </>
  );
}
