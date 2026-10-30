"use client";

import { useRouter } from "next/navigation";
import { useState, type FormEvent } from "react";
import { api, messageFor } from "@/lib/api";

export default function SignInPage() {
  const router = useRouter();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await api("/api/session", {
        method: "POST",
        body: { email: String(form.get("email") ?? ""), password: String(form.get("password") ?? "") },
      });
      router.replace("/");
    } catch (failure) {
      setError(messageFor(failure));
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-panel px-5 py-12">
      <main className="flex w-full max-w-[460px] flex-col gap-6 rounded-2xl border-2 border-ink bg-paper px-9 py-10">
        <div className="flex flex-col gap-2">
          <h1 className="text-[34px] font-bold leading-tight">Espaço de agentes</h1>
          <p className="text-[19px]">Entre com o e-mail e a senha da sua conta.</p>
        </div>
        <form onSubmit={submit} className="flex flex-col gap-5" noValidate>
          <div className="flex flex-col gap-2">
            <label htmlFor="email" className="text-lg font-bold">E-mail</label>
            <input id="email" name="email" type="email" autoComplete="username" required
              className="rounded-[10px] border-2 border-ink px-4 py-3.5 text-[19px] font-bold" />
          </div>
          <div className="flex flex-col gap-2">
            <label htmlFor="password" className="text-lg font-bold">Senha</label>
            <input id="password" name="password" type="password" autoComplete="current-password" required
              className="rounded-[10px] border-2 border-ink px-4 py-3.5 text-[19px] font-bold" />
            <p className="text-base">A senha tem 15 caracteres ou mais. Uma frase com espaços funciona bem.</p>
          </div>
          {error && (
            <p role="alert" className="rounded-[10px] border-2 border-danger-ink bg-danger-bg px-4 py-3 text-lg font-bold text-danger-ink">
              {error}
            </p>
          )}
          <button type="submit" disabled={busy}
            className="rounded-[10px] bg-action px-6 py-4 text-xl font-bold text-paper disabled:opacity-60">
            {busy ? "Entrando..." : "Entrar"}
          </button>
        </form>
        <p className="border-t-2 border-line pt-5 text-[17px]">
          Esqueceu a senha? Peça para um administrador da sua empresa criar uma nova.
        </p>
      </main>
    </div>
  );
}
