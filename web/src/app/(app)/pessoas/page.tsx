"use client";

import { NewPersonForm } from "@/components/new-person-form";
import { PersonRow } from "@/components/person-row";
import { useMe } from "@/components/shell";
import { Card, ErrorNote, PageTitle } from "@/components/ui";
import { messageFor } from "@/lib/api";
import type { AgentSummary, Person } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";

export default function PeoplePage() {
  const me = useMe();
  const isAdmin = me?.role === "admin";
  const people = usePoll<Person[]>(isAdmin ? "/api/users" : null);
  const agents = usePoll<AgentSummary[]>(isAdmin ? "/api/agents" : null);

  if (me && !isAdmin) {
    return (
      <>
        <PageTitle>Pessoas</PageTitle>
        <p className="text-lg">Só administradores podem ver e mudar as contas.</p>
      </>
    );
  }

  const activeAgents = (agents.data ?? []).filter((agent) => !agent.archived);
  return (
    <>
      <PageTitle subtitle="Contas, papéis e o acesso de cada pessoa aos agentes.">Pessoas</PageTitle>
      <section className="flex flex-col gap-4" aria-labelledby="new-person-title">
        <h2 id="new-person-title" className="text-[28px] font-bold">Nova conta</h2>
        <Card><NewPersonForm onCreated={people.refresh} /></Card>
      </section>
      <section className="flex flex-col gap-4" aria-labelledby="people-title">
        <h2 id="people-title" className="text-[28px] font-bold">Contas</h2>
        {people.error ? <ErrorNote>{messageFor(people.error)}</ErrorNote> : null}
        {people.loading ? <p className="text-lg">Carregando...</p> : null}
        <ul className="flex flex-col gap-4">
          {(people.data ?? []).map((person) => (
            <PersonRow key={person.id} person={person} agents={activeAgents} isSelf={person.id === me?.id}
              onChanged={people.refresh} />
          ))}
        </ul>
      </section>
    </>
  );
}
