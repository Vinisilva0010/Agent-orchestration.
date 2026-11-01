import Link from "next/link";
import { agentStatus, formatMoney } from "@/lib/format";
import type { AgentSummary } from "@/lib/types";
import { Badge } from "./ui";

export function AgentCard({ agent }: { agent: AgentSummary }) {
  const status = agentStatus(agent);
  return (
    <li className="flex flex-col gap-3 rounded-2xl border-2 border-ink bg-paper p-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h3 className="text-2xl font-bold">{agent.display_name}</h3>
        <Badge tone={status.tone}>{status.label}</Badge>
      </div>
      {agent.description ? <p className="whitespace-pre-wrap break-words text-lg">{agent.description}</p> : null}
      <p className="text-base font-bold">
        Gasto deste mês: {formatMoney(agent.month_spent_micros)}
        {agent.month_limit_micros ? ` de ${formatMoney(agent.month_limit_micros)}` : ""}
      </p>
      <Link href={`/agentes/${encodeURIComponent(agent.id)}`}
        className="self-start rounded-[10px] border-2 border-ink px-5 py-2.5 text-lg font-bold no-underline">
        {agent.can_use ? "Abrir e dar tarefa" : "Abrir"}
      </Link>
    </li>
  );
}
