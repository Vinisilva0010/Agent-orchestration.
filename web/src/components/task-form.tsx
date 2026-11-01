"use client";

import { useState, type FormEvent } from "react";
import { api, messageFor } from "@/lib/api";
import { TASK_MAX_CHARS, formatMoney, taskLength } from "@/lib/format";
import type { AgentSummary } from "@/lib/types";
import { ErrorNote, InfoNote, buttonPrimary, inputClass } from "./ui";

type Props = {
  /** Agents the person may use. With one agent there is nothing to choose. */
  agents: AgentSummary[];
  onSent?: () => void;
};

/** Writes a task and sends it to an agent. */
export function TaskForm({ agents, onSent }: Props) {
  const [agentId, setAgentId] = useState(agents[0]?.id ?? "");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState(false);
  const agent = agents.find((candidate) => candidate.id === agentId) ?? agents[0];
  const length = taskLength(text);
  const tooLong = length > TASK_MAX_CHARS;

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setSent(false);
    if (!agent) {
      return;
    }
    if (length === 0) {
      setError("Escreva o que o agente deve fazer.");
      return;
    }
    if (tooLong) {
      setError(`A tarefa pode ter no máximo ${TASK_MAX_CHARS} caracteres.`);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api(`/api/agents/${encodeURIComponent(agent.id)}/tasks`, {
        method: "POST",
        body: { task: text },
      });
      setText("");
      setSent(true);
      onSent?.();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
    }
  }

  if (!agent) {
    return <p className="text-lg font-bold">Você ainda não tem nenhum agente para dar tarefas.</p>;
  }

  return (
    <form onSubmit={submit} className="flex flex-col gap-4" noValidate>
      {agents.length > 1 ? (
        <div className="flex flex-col gap-2">
          <label htmlFor="task-agent" className="text-lg font-bold">Agente</label>
          <select id="task-agent" value={agent.id} onChange={(event) => setAgentId(event.target.value)}
            className={inputClass}>
            {agents.map((candidate) => (
              <option key={candidate.id} value={candidate.id}>{candidate.display_name}</option>
            ))}
          </select>
        </div>
      ) : null}
      <div className="flex flex-col gap-2">
        <label htmlFor="task-text" className="text-lg font-bold">O que o agente deve fazer?</label>
        <textarea id="task-text" value={text} onChange={(event) => setText(event.target.value)} rows={5}
          aria-describedby="task-help" className={`${inputClass} resize-y`} />
        <p id="task-help" className={`text-base font-bold ${tooLong ? "text-danger-ink" : ""}`}>
          {length} de {TASK_MAX_CHARS} caracteres. Limite de gasto desta tarefa: {formatMoney(agent.task_budget_micros)}.
        </p>
      </div>
      {error ? <ErrorNote>{error}</ErrorNote> : null}
      {sent ? <InfoNote>Tarefa enviada. Ela começa em alguns segundos.</InfoNote> : null}
      <button type="submit" disabled={busy} className={`${buttonPrimary} self-start`}>
        {busy ? "Enviando..." : "Enviar tarefa"}
      </button>
    </form>
  );
}
