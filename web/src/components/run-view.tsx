"use client";

import { OUTCOME_LABELS, STATUS_LABELS, formatTime, statusTone, toolLabel } from "@/lib/format";
import type { RunDetail } from "@/lib/types";
import { ApprovalCard } from "./approval-card";
import { Badge, Card, ErrorNote, IrreversibleTag, SpendLine } from "./ui";

const STOP_REASONS: Partial<Record<RunDetail["status"], string>> = {
  failed: "A tarefa não foi concluída. Ela pode ter sido cancelada ou ter encontrado um erro.",
  budget_exceeded: "O dinheiro reservado para esta tarefa acabou antes do fim.",
  max_steps: "O agente chegou ao limite de passos sem terminar a tarefa.",
  awaiting_budget: "A tarefa está parada porque o orçamento acabou. Um administrador pode liberar mais na página Gastos.",
};

/**
 * One task in detail. Everything written by the model or by tools is shown
 * as plain text.
 */
export function RunView({ run, onChanged }: { run: RunDetail; onChanged: () => void }) {
  return (
    <div className="flex flex-col gap-6">
      <Card>
        <div className="flex flex-wrap items-center gap-3">
          <Badge tone={statusTone(run.status)}>{STATUS_LABELS[run.status]}</Badge>
          <span className="text-base font-bold">
            Começou em {formatTime(run.created_at)}
            {run.requested_by ? `, pedida por ${run.requested_by}` : ""}
          </span>
        </div>
        <SpendLine title="Gasto desta tarefa" spent={run.cost_micros} limit={run.budget_micros} />
      </Card>

      {run.approvals.length > 0 ? (
        <ApprovalCard runId={run.id} agentName={run.agent_name ?? run.agent_id} task={run.task}
          approvals={run.approvals} canDecide={run.can_decide} onDecided={onChanged} />
      ) : null}

      <section className="flex flex-col gap-4" aria-labelledby="conversation-title">
        <h2 id="conversation-title" className="text-[28px] font-bold">Conversa</h2>
        <ol className="flex flex-col gap-3">
          {run.conversation.map((entry, index) => (
            <li key={index}
              className={`flex flex-col gap-2 rounded-2xl border-2 p-5 ${entry.role === "agent" ? "border-ink bg-paper" : "border-line bg-panel"}`}>
              <span className="text-base font-bold">{entry.role === "agent" ? "Agente" : "Pedido"}</span>
              {entry.text ? <p className="whitespace-pre-wrap break-words text-lg">{entry.text}</p> : null}
              {entry.tools.length > 0 ? (
                <p className="text-base font-bold">Usou: {entry.tools.map(toolLabel).join(", ")}</p>
              ) : null}
            </li>
          ))}
        </ol>
      </section>

      <section className="flex flex-col gap-4" aria-labelledby="activity-title">
        <h2 id="activity-title" className="text-[28px] font-bold">O que o agente fez</h2>
        {run.activity.length === 0 ? (
          <p className="text-lg font-bold">O agente ainda não usou nenhuma ferramenta nesta tarefa.</p>
        ) : (
          <ol className="flex flex-col gap-3">
            {run.activity.map((item) => (
              <li key={item.call_id} className="flex flex-col gap-2 rounded-2xl border-2 border-ink p-5">
                <div className="flex flex-wrap items-center gap-3">
                  <span className="text-lg font-bold">{toolLabel(item.tool)}</span>
                  <Badge tone={item.outcome === "done" ? "done" : item.outcome === "waiting" ? "waiting" : "stopped"}>
                    {OUTCOME_LABELS[item.outcome]}
                  </Badge>
                  {item.irreversible ? <IrreversibleTag /> : null}
                </div>
                {item.finished_at ? (
                  <span className="text-base">Terminou em {formatTime(item.finished_at)}</span>
                ) : null}
                {item.result ? (
                  <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md border-2 border-line bg-panel p-3 font-mono text-base">
                    {item.result}
                  </pre>
                ) : null}
              </li>
            ))}
          </ol>
        )}
      </section>

      <section className="flex flex-col gap-4" aria-labelledby="result-title">
        <h2 id="result-title" className="text-[28px] font-bold">Resultado</h2>
        {run.final_text ? (
          <Card><p className="whitespace-pre-wrap break-words text-lg">{run.final_text}</p></Card>
        ) : run.status === "completed" ? (
          <p className="text-lg font-bold">O agente terminou sem escrever uma resposta.</p>
        ) : (
          <p className="text-lg font-bold">Ainda não há resultado.</p>
        )}
        {STOP_REASONS[run.status] ? <ErrorNote>{STOP_REASONS[run.status]}</ErrorNote> : null}
        {run.error ? (
          <details className="rounded-[10px] border-2 border-ink bg-panel px-4 py-3">
            <summary className="cursor-pointer text-lg font-bold">Detalhes técnicos do problema</summary>
            <p className="mt-3 whitespace-pre-wrap break-words font-mono text-base">{run.error}</p>
          </details>
        ) : null}
      </section>
    </div>
  );
}
