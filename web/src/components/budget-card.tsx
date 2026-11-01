"use client";

import Link from "next/link";
import { useState, type FormEvent } from "react";
import { api, messageFor } from "@/lib/api";
import { checkNewTaskBudget, formatMoney } from "@/lib/format";
import type { WaitingRun } from "@/lib/types";
import { Card, ErrorNote, InfoNote, SpendLine, buttonDanger, buttonPrimary, inputClass } from "./ui";

/**
 * A task paused because its budget ran out. Administrators may give it more
 * and let it go on; people who manage the agent may cancel it.
 */
export function BudgetCard({ run, isAdmin, onChanged }: { run: WaitingRun; isAdmin: boolean; onChanged: () => void }) {
  const [amount, setAmount] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const fieldId = `budget-${run.id}`;

  async function raise(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const checked = checkNewTaskBudget(amount, run.budget_micros);
    if ("error" in checked) {
      setError(checked.error);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api(`/api/runs/${encodeURIComponent(run.id)}/budget`, {
        method: "POST",
        body: { budget_micros: checked.micros },
      });
      setDone(`Limite aumentado para ${formatMoney(checked.micros)}. A tarefa continua em alguns segundos.`);
      onChanged();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
    }
  }

  async function cancel() {
    setBusy(true);
    setError(null);
    try {
      await api(`/api/runs/${encodeURIComponent(run.id)}/cancel`, { method: "POST" });
      setDone("Pedido de cancelamento enviado. A tarefa para em alguns segundos.");
      onChanged();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
      setConfirmCancel(false);
    }
  }

  return (
    <Card className="border-warn-ink">
      <div className="flex flex-wrap items-center gap-3">
        <span className="inline-flex rounded-md border-2 border-warn-ink bg-warn-bg px-3 py-1 text-base font-bold text-warn-ink">
          Parado por orçamento
        </span>
        <Link href={`/agentes/${encodeURIComponent(run.agent_id)}`} className="text-lg font-bold underline">
          {run.agent_name ?? run.agent_id}
        </Link>
      </div>
      <p className="line-clamp-4 whitespace-pre-wrap break-words text-lg">{run.task}</p>
      <SpendLine title="Gasto desta tarefa" spent={run.cost_micros ?? 0} limit={run.budget_micros} />
      {error ? <ErrorNote>{error}</ErrorNote> : null}
      {done ? (
        <InfoNote>{done}</InfoNote>
      ) : (
        <div className="flex flex-col gap-4 border-t-2 border-line pt-4">
          {isAdmin ? (
            <form onSubmit={raise} className="flex flex-col gap-3" noValidate>
              <label htmlFor={fieldId} className="text-lg font-bold">Novo limite desta tarefa (em US$)</label>
              <input id={fieldId} inputMode="decimal" autoComplete="off" value={amount}
                onChange={(event) => setAmount(event.target.value)}
                className={`${inputClass} max-w-xs`} />
              <p className="text-base">Por exemplo: 5,00. Hoje o limite é {formatMoney(run.budget_micros)}. O máximo por tarefa é {formatMoney(100_000_000)}.</p>
              <button type="submit" disabled={busy} className={`${buttonPrimary} self-start`}>Liberar e continuar</button>
            </form>
          ) : (
            <p className="text-lg">Só um administrador pode aumentar o limite de uma tarefa.</p>
          )}
          {run.can_cancel ? (
            confirmCancel ? (
              <div className="flex flex-wrap items-center gap-3">
                <span className="text-lg">Cancelar mesmo? A tarefa para e não pode continuar depois.</span>
                <button type="button" disabled={busy} onClick={cancel} className={buttonDanger}>Sim, cancelar</button>
                <button type="button" disabled={busy} onClick={() => setConfirmCancel(false)}
                  className="text-lg font-bold underline">Não</button>
              </div>
            ) : (
              <button type="button" disabled={busy} onClick={() => setConfirmCancel(true)}
                className={`${buttonDanger} self-start`}>Cancelar a tarefa</button>
            )
          ) : null}
        </div>
      )}
    </Card>
  );
}
