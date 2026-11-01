"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { ROLE_LABELS, api, goToSignIn, type Me } from "@/lib/api";
import { countDecisions } from "@/lib/format";
import type { WaitingRun } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";

type NavItem = { href: string; label: string; adminOnly?: boolean; count?: "decisions" };

export const NAV: NavItem[] = [
  { href: "/", label: "Início" },
  { href: "/agentes", label: "Agentes" },
  { href: "/aprovacoes", label: "Aprovações", count: "decisions" },
  { href: "/gastos", label: "Gastos" },
  { href: "/pessoas", label: "Pessoas", adminOnly: true },
  { href: "/configuracoes", label: "Configurações", adminOnly: true },
];

const MeContext = createContext<Me | null>(null);

/** The signed-in person, read once by the shell. */
export function useMe(): Me | null {
  return useContext(MeContext);
}

function isCurrent(pathname: string, href: string): boolean {
  return href === "/" ? pathname === "/" : pathname === href || pathname.startsWith(`${href}/`);
}

export function Shell({ children }: { children: ReactNode }) {
  const pathname = usePathname();
  const [me, setMe] = useState<Me | null>(null);
  const waiting = usePoll<WaitingRun[]>("/api/runs");
  const decisions = waiting.data ? countDecisions(waiting.data) : 0;

  useEffect(() => {
    let active = true;
    api<Me>("/api/me")
      .then((value) => {
        if (active) setMe(value);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  async function signOut() {
    try {
      await api("/api/session", { method: "DELETE" });
    } finally {
      goToSignIn();
    }
  }

  const items = NAV.filter((item) => !item.adminOnly || me?.role === "admin");

  return (
    <MeContext.Provider value={me}>
      <div className="flex min-h-screen flex-wrap">
        <nav aria-label="Menu principal"
          className="flex max-w-full flex-[1_1_240px] flex-col gap-7 border-r-2 border-ink bg-panel px-5 py-7">
          <div className="px-3 text-2xl font-bold">Espaço de agentes</div>
          <div className="flex flex-col gap-1.5">
            {items.map((item) => {
              const current = isCurrent(pathname, item.href);
              const count = item.count === "decisions" ? decisions : 0;
              return (
                <Link key={item.href} href={item.href} aria-current={current ? "page" : undefined}
                  className={`flex items-center justify-between gap-3 rounded-[10px] px-4 py-3 text-[19px] font-bold no-underline ${current ? "bg-ink text-paper" : "text-ink"}`}>
                  <span>{item.label}</span>
                  {count > 0 ? (
                    <span aria-label={`${count} esperando decisão`}
                      className="min-w-8 rounded-full border-2 border-warn-ink bg-warn-bg px-2 text-center text-base font-bold text-warn-ink">
                      {count}
                    </span>
                  ) : null}
                </Link>
              );
            })}
          </div>
          <div className="mt-auto flex flex-col gap-1.5 px-3">
            <div className="text-lg font-bold">{me?.display_name ?? " "}</div>
            <div className="text-base">{me ? ROLE_LABELS[me.role] : " "}</div>
            <button type="button" onClick={signOut} className="self-start text-[17px] font-bold underline">
              Sair
            </button>
          </div>
        </nav>
        <main className="flex min-w-0 max-w-[1120px] flex-[999_1_560px] flex-col gap-10 px-6 pb-16 pt-12 sm:px-12">
          {children}
        </main>
      </div>
    </MeContext.Provider>
  );
}
