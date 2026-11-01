"use client";

import { useState } from "react";
import { api, messageFor } from "@/lib/api";
import { prettyJson, toolDescription, toolLabel } from "@/lib/format";
import type { Approval } from "@/lib/types";
import { Card, ErrorNote, InfoNote, IrreversibleTag, buttonDanger, buttonPrimary } from "./ui";

type Props = {
  runId: string;
  agentName: string;
  task: string;
  approvals: Approval[];
  canDecide: boolean;
  /** Called after the decision was accepted, to reload the page's data. */
  onDecided: () => void;
};

/**
 * The actions one task waits on, and one decision for all of them. The
 * server rejects every waiting action that is not approved, so a single
 * answer covers the whole set. Model text is shown only as text.
 */
export function ApprovalCard({ runId, agentName, task, approvals, canDecide, onDecided }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState<"approved" | "rejected" | null>(null);
  const irreversible = approvals.some((approval) => approval.irreversible);

  async function decide(approve: boolean) {
    setBusy(true);
    setError(null);
    try {
      await api(`/api/runs/${encodeURIComponent(runId)}/decision`, {
        method: "POST",
        body: { approve: approve ? approvals.map((approval) => approval.call_id) : [] },
      });
      setSent(approve ? "approved" : "rejected");
      onDecided();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className={irreversible ? "border-danger-ink" : ""}>
      <div className="flex flex-wrap items-center gap-3">
        {irreversible ? <IrreversibleTag /> : null}
        <span className="text-lg font-bold">{agentName}</span>
      </div>
      <div className="flex flex-col gap-1">
        <h3 className="text-base font-bold">Tarefa</h3>
        <p className="whitespace-pre-wrap break-words text-lg">{task}</p>
      </div>
      {approvals.length > 1 ? (
        <InfoNote>Esta tarefa pede {approvals.length} ações. A sua decisão vale para todas.</InfoNote>
      ) : null}
      {approvals.map((approval) => (
        <div key={approval.call_id} className="flex flex-col gap-4 border-t-2 border-line pt-4">
          <div className="flex flex-col gap-1">
            <h3 className="text-xl font-bold">O que vai acontecer</h3>
            <p className="text-lg font-bold">{toolLabel(approval.tool)}</p>
            <p className="whitespace-pre-wrap break-words text-lg">
              {toolDescription(approval.tool, approval.description) || "A ferramenta não tem descrição. Veja os detalhes técnicos antes de decidir."}
            </p>
            {approval.irreversible ? (
              <p className="text-lg font-bold text-danger-ink">Depois de feita, esta ação não pode ser desfeita.</p>
            ) : null}
          </div>
          <div className="flex flex-col gap-1">
            <h3 className="text-xl font-bold">Por que o agente pediu</h3>
            <p className="whitespace-pre-wrap break-words text-lg">
              {approval.reason || "O agente não explicou."}
            </p>
            <p className="text-base">Este texto foi escrito pelo agente. Confira se faz sentido antes de aprovar.</p>
          </div>
          <details className="rounded-[10px] border-2 border-ink bg-panel px-4 py-3">
            <summary className="cursor-pointer text-lg font-bold">Detalhes técnicos</summary>
            <dl className="mt-3 flex flex-col gap-2">
              <dt className="text-base font-bold">Ferramenta</dt>
              <dd className="break-words font-mono text-base">{approval.tool}</dd>
              <dt className="text-base font-bold">Entrada (JSON)</dt>
              <dd>
                <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-md border-2 border-line bg-paper p-3 font-mono text-base">
                  {prettyJson(approval.input)}
                </pre>
              </dd>
            </dl>
          </details>
        </div>
      ))}
      {error ? <ErrorNote>{error}</ErrorNote> : null}
      {sent ? (
        <InfoNote>
          {sent === "approved" ? "Aprovação enviada." : "Recusa enviada."} O agente continua em alguns segundos.
        </InfoNote>
      ) : canDecide ? (
        <div className="flex flex-col gap-3 border-t-2 border-line pt-4">
          <div className="flex flex-wrap gap-3">
            <button type="button" disabled={busy} onClick={() => decide(true)} className={buttonPrimary}>
              Aprovar
            </button>
            <button type="button" disabled={busy} onClick={() => decide(false)} className={buttonDanger}>
              Recusar
            </button>
          </div>
          <p className="text-base font-bold">
            Sua decisão fica guardada no registro de auditoria, com o seu nome e a hora.
          </p>
        </div>
      ) : (
        <p className="text-lg font-bold">Você pode ver esta ação, mas não pode decidir sobre ela.</p>
      )}
    </Card>
  );
}
