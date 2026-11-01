"use client";

import { useState } from "react";
import { ROLE_LABELS, api, messageFor, type Role } from "@/lib/api";
import { LEVEL_LABELS } from "@/lib/format";
import type { AgentSummary, Grant, Person } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";
import { Badge, ErrorNote, buttonDanger, buttonSecondary, inputClass } from "./ui";

type Props = { person: Person; agents: AgentSummary[]; isSelf: boolean; onChanged: () => void };

/** One account: role, active or not, and permissions on each agent. */
export function PersonRow({ person, agents, isSelf, onChanged }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDisable, setConfirmDisable] = useState(false);
  const [open, setOpen] = useState(false);
  const grants = usePoll<Grant[]>(open ? `/api/users/${person.id}/agents` : null);
  const roleId = `role-${person.id}`;

  async function change(body: { role?: Role; disabled?: boolean }) {
    setBusy(true);
    setError(null);
    try {
      await api(`/api/users/${person.id}`, { method: "PATCH", body });
      onChanged();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
      setConfirmDisable(false);
    }
  }

  async function setLevel(agentId: string, level: string) {
    setBusy(true);
    setError(null);
    const path = `/api/users/${person.id}/agents/${encodeURIComponent(agentId)}`;
    try {
      if (level === "none") {
        await api(path, { method: "DELETE" });
      } else {
        await api(path, { method: "PUT", body: { level } });
      }
      grants.refresh();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
    }
  }

  const levelOf = (agentId: string) => grants.data?.find((grant) => grant.agent_id === agentId)?.level ?? "none";

  return (
    <li className="flex flex-col gap-4 rounded-2xl border-2 border-ink bg-paper p-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-col gap-1">
          <span className="text-xl font-bold">{person.display_name}{isSelf ? " (você)" : ""}</span>
          <span className="break-all text-lg">{person.email}</span>
        </div>
        <Badge tone={person.disabled ? "stopped" : "done"}>{person.disabled ? "Desativada" : "Ativa"}</Badge>
      </div>

      <div className="flex flex-wrap items-end gap-4">
        <div className="flex flex-col gap-2">
          <label htmlFor={roleId} className="text-base font-bold">Papel</label>
          <select id={roleId} value={person.role} disabled={busy}
            onChange={(event) => change({ role: event.target.value as Role })} className={inputClass}>
            {(Object.keys(ROLE_LABELS) as Role[]).map((role) => (
              <option key={role} value={role}>{ROLE_LABELS[role]}</option>
            ))}
          </select>
        </div>
        {person.disabled ? (
          <button type="button" disabled={busy} onClick={() => change({ disabled: false })} className={buttonSecondary}>
            Reativar
          </button>
        ) : isSelf ? null : confirmDisable ? (
          <div className="flex flex-wrap items-center gap-3">
            <span className="text-lg">Desativar? A pessoa sai na hora de todas as sessões.</span>
            <button type="button" disabled={busy} onClick={() => change({ disabled: true })} className={buttonDanger}>
              Sim, desativar
            </button>
            <button type="button" onClick={() => setConfirmDisable(false)} className="text-lg font-bold underline">Não</button>
          </div>
        ) : (
          <button type="button" disabled={busy} onClick={() => setConfirmDisable(true)} className={buttonDanger}>
            Desativar
          </button>
        )}
      </div>

      {error ? <ErrorNote>{error}</ErrorNote> : null}

      {person.role === "admin" ? (
        <p className="text-base">Administradores podem usar e gerenciar todos os agentes.</p>
      ) : (
        <details open={open} onToggle={(event) => setOpen(event.currentTarget.open)}
          className="rounded-[10px] border-2 border-ink bg-panel px-4 py-3">
          <summary className="cursor-pointer text-lg font-bold">Permissões por agente</summary>
          {person.role === "viewer" ? (
            <p className="mt-3 text-base">Quem só lê vê os agentes liberados, mas nunca dá tarefas nem decide.</p>
          ) : null}
          {open && !grants.data ? <p className="mt-3 text-lg">Carregando...</p> : null}
          {grants.data ? (
            <ul className="mt-3 flex flex-col gap-3">
              {agents.length === 0 ? <li className="text-lg">Nenhum agente cadastrado.</li> : null}
              {agents.map((agent) => {
                const id = `grant-${person.id}-${agent.id}`;
                return (
                  <li key={agent.id} className="flex flex-wrap items-center justify-between gap-3">
                    <label htmlFor={id} className="text-lg font-bold">{agent.display_name}</label>
                    <select id={id} value={levelOf(agent.id)} disabled={busy}
                      onChange={(event) => setLevel(agent.id, event.target.value)} className={inputClass}>
                      <option value="none">Sem acesso</option>
                      <option value="use">{LEVEL_LABELS.use}</option>
                      <option value="manage">{LEVEL_LABELS.manage}</option>
                    </select>
                  </li>
                );
              })}
            </ul>
          ) : null}
        </details>
      )}
    </li>
  );
}
