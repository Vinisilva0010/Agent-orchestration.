import type { ReactNode } from "react";
import { formatMoney, percentUsed, type Tone } from "@/lib/format";

const TONES: Record<Tone, string> = {
  neutral: "border-ink bg-panel text-ink",
  working: "border-action bg-paper text-action",
  waiting: "border-warn-ink bg-warn-bg text-warn-ink",
  stopped: "border-danger-ink bg-danger-bg text-danger-ink",
  done: "border-ink bg-paper text-ink",
};

export function Badge({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span className={`inline-flex items-center rounded-full border-2 px-3 py-1 text-base font-bold ${TONES[tone]}`}>
      {children}
    </span>
  );
}

/** Red label for actions that cannot be undone. */
export function IrreversibleTag() {
  return (
    <span className="inline-flex items-center rounded-md border-2 border-danger-ink bg-danger-bg px-3 py-1 text-base font-bold text-danger-ink">
      Ação sem volta
    </span>
  );
}

/**
 * A bar drawn with SVG attributes (the CSP forbids inline styles).
 */
export function ProgressBar({ value, label }: { value: number; label: string }) {
  const width = Math.min(100, Math.max(0, value));
  return (
    <svg width="100%" height="14" viewBox="0 0 100 14" preserveAspectRatio="none" role="img" aria-label={label}
      className="block">
      <rect x="0" y="0" width="100" height="14" rx="3" className="fill-line" />
      <rect x="0" y="0" width={width} height="14" rx="3" className={width >= 90 ? "fill-stopped" : "fill-ink"} />
    </svg>
  );
}

/** "US$ 3,12 de US$ 20,00" with a bar, or just the spend when there is no limit. */
export function SpendLine({ title, spent, limit }: { title: string; spent: number; limit: number | null }) {
  const used = percentUsed(spent, limit);
  return (
    <div className="flex flex-col gap-2">
      <div className="text-base font-bold">{title}</div>
      <div className="text-2xl font-bold">
        {formatMoney(spent)}
        {limit ? <span className="text-xl"> de {formatMoney(limit)}</span> : null}
      </div>
      {limit ? <ProgressBar value={used} label={`${used}% usado`} /> : <div className="text-base">Sem limite definido.</div>}
    </div>
  );
}

export function ErrorNote({ children }: { children: ReactNode }) {
  return (
    <p role="alert" className="rounded-[10px] border-2 border-danger-ink bg-danger-bg px-4 py-3 text-lg font-bold text-danger-ink">
      {children}
    </p>
  );
}

export function InfoNote({ children }: { children: ReactNode }) {
  return (
    <p role="status" className="rounded-[10px] border-2 border-ink bg-panel px-4 py-3 text-lg font-bold">
      {children}
    </p>
  );
}

export function PageTitle({ children, subtitle }: { children: ReactNode; subtitle?: ReactNode }) {
  return (
    <header className="flex flex-col gap-2">
      <h1 className="text-[42px] font-bold leading-tight">{children}</h1>
      {subtitle ? <p className="text-xl font-bold">{subtitle}</p> : null}
    </header>
  );
}

export function Card({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <section className={`flex flex-col gap-4 rounded-2xl border-2 border-ink bg-paper p-6 ${className}`}>{children}</section>;
}

export const buttonPrimary =
  "rounded-[10px] bg-action px-6 py-3.5 text-xl font-bold text-paper disabled:opacity-60";
export const buttonSecondary =
  "rounded-[10px] border-2 border-ink bg-paper px-6 py-3 text-xl font-bold text-ink disabled:opacity-60";
export const buttonDanger =
  "rounded-[10px] border-2 border-danger-ink bg-danger-bg px-6 py-3 text-xl font-bold text-danger-ink disabled:opacity-60";
export const inputClass = "rounded-[10px] border-2 border-ink px-4 py-3 text-[19px] font-bold";
