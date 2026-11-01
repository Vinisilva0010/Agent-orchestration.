"use client";

import { useState, type FormEvent } from "react";
import { api, messageFor } from "@/lib/api";
import { formatMoney, parseMoney, percentUsed } from "@/lib/format";
import type { AgentSummary } from "@/lib/types";
import { ProgressBar, buttonPrimary, buttonSecondary, inputClass } from "./ui";

/** One agent's spend this month, with "Mudar limite" for administrators. */
export function AgentLimitRow({ agent, isAdmin, onChanged }: { agent: AgentSummary; isAdmin: boolean; onChanged: () => void }) {
  const [editing, setEditing] = useState(false);
  const [amount, setAmount] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const used = percentUsed(agent.month_spent_micros, agent.month_limit_micros);
  const fieldId = `limit-${agent.id}`;

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const micros = parseMoney(amount);
    if (micros === null) {
      setError("Digite um valor em dólares, por exemplo 20,00.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api(`/api/agents/${encodeURIComponent(agent.id)}/budget`, {
        method: "PUT",
        body: { monthly_budget_micros: micros },
      });
      setEditing(false);
      setAmount("");
      onChanged();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <tr className="border-t-2 border-line align-top">
      <th scope="row" className="py-4 pr-4 text-left text-lg font-bold">{agent.display_name}</th>
      <td className="py-4 pr-4 text-lg">{formatMoney(agent.month_spent_micros)}</td>
      <td className="py-4 pr-4 text-lg">
        <div className="flex min-w-40 flex-col gap-2">
          <span>{agent.month_limit_micros ? formatMoney(agent.month_limit_micros) : "Sem limite"}</span>
          {agent.month_limit_micros ? <ProgressBar value={used} label={`${used}% usado`} /> : null}
        </div>
      </td>
      {isAdmin ? (
        <td className="py-4 text-lg">
          {editing ? (
            <form onSubmit={save} className="flex flex-col gap-2" noValidate>
              <label htmlFor={fieldId} className="text-base font-bold">Novo limite do mês (US$)</label>
              <input id={fieldId} inputMode="decimal" autoComplete="off" value={amount}
                onChange={(event) => setAmount(event.target.value)} className={`${inputClass} max-w-48`} />
              {error ? <p role="alert" className="text-base font-bold text-danger-ink">{error}</p> : null}
              <div className="flex flex-wrap gap-2">
                <button type="submit" disabled={busy} className={buttonPrimary}>Salvar</button>
                <button type="button" disabled={busy} onClick={() => { setEditing(false); setError(null); }}
                  className={buttonSecondary}>Voltar</button>
              </div>
            </form>
          ) : (
            <button type="button" onClick={() => setEditing(true)} className={buttonSecondary}>Mudar limite</button>
          )}
        </td>
      ) : null}
    </tr>
  );
}
