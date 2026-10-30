"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState, type ReactNode } from "react";
import { ROLE_LABELS, api, goToSignIn, type Me } from "@/lib/api";

const NAV = [{ href: "/", label: "Início" }];

/** The signed-in person, from the server. */
export function useMe(): Me | null {
  const [me, setMe] = useState<Me | null>(null);
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
  return me;
}

export function Shell({ children }: { children: ReactNode }) {
  const pathname = usePathname();
  const me = useMe();

  async function signOut() {
    try {
      await api("/api/session", { method: "DELETE" });
    } finally {
      goToSignIn();
    }
  }

  return (
    <div className="flex min-h-screen flex-wrap">
      <nav aria-label="Menu principal"
        className="flex max-w-full flex-[1_1_240px] flex-col gap-7 border-r-2 border-ink bg-panel px-5 py-7">
        <div className="px-3 text-2xl font-bold">Espaço de agentes</div>
        <div className="flex flex-col gap-1.5">
          {NAV.map((item) => {
            const current = pathname === item.href;
            return (
              <Link key={item.href} href={item.href} aria-current={current ? "page" : undefined}
                className={`rounded-[10px] px-4 py-3 text-[19px] font-bold no-underline ${current ? "bg-ink text-paper" : "text-ink"}`}>
                {item.label}
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
      <main className="flex min-w-0 max-w-[1120px] flex-[999_1_560px] flex-col gap-10 px-12 pb-16 pt-12">
        {children}
      </main>
    </div>
  );
}
