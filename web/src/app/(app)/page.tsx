"use client";

import { useMe } from "@/components/shell";

export default function HomePage() {
  const me = useMe();
  return (
    <>
      <h1 className="text-[42px] font-bold leading-tight">Início</h1>
      <p className="text-xl font-bold">{me ? `Olá, ${me.display_name}.` : "Carregando..."}</p>
    </>
  );
}
