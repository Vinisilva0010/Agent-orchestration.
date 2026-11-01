"use client";

import { useState, type FormEvent } from "react";
import { ROLE_LABELS, api, messageFor, type Role } from "@/lib/api";
import { PASSWORD_MAX, PASSWORD_MIN } from "@/lib/format";
import { ErrorNote, InfoNote, buttonPrimary, inputClass } from "./ui";

/**
 * Creates an account. The first password is read straight from the form
 * when sending and never kept in the page's state.
 */
export function NewPersonForm({ onCreated }: { onCreated: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<string | null>(null);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = event.currentTarget;
    const data = new FormData(form);
    const name = String(data.get("display_name") ?? "").trim();
    const email = String(data.get("email") ?? "").trim();
    const password = String(data.get("password") ?? "");
    setCreated(null);
    if (!name || !email) {
      setError("Preencha o nome e o e-mail.");
      return;
    }
    const length = Array.from(password).length;
    if (length < PASSWORD_MIN || length > PASSWORD_MAX) {
      setError(`A senha precisa ter de ${PASSWORD_MIN} a ${PASSWORD_MAX} caracteres.`);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api("/api/users", {
        method: "POST",
        body: { display_name: name, email, role: String(data.get("role")), password },
      });
      form.reset();
      setCreated(`Conta de ${name} criada. Entregue a senha à pessoa por um meio seguro.`);
      onCreated();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={submit} className="flex flex-col gap-4" noValidate autoComplete="off">
      <div className="grid gap-4 md:grid-cols-2">
        <div className="flex flex-col gap-2">
          <label htmlFor="new-name" className="text-lg font-bold">Nome</label>
          <input id="new-name" name="display_name" maxLength={100} className={inputClass} />
        </div>
        <div className="flex flex-col gap-2">
          <label htmlFor="new-email" className="text-lg font-bold">E-mail</label>
          <input id="new-email" name="email" type="email" maxLength={254} className={inputClass} />
        </div>
        <div className="flex flex-col gap-2">
          <label htmlFor="new-role" className="text-lg font-bold">Papel</label>
          <select id="new-role" name="role" defaultValue="member" className={inputClass}>
            {(Object.keys(ROLE_LABELS) as Role[]).map((role) => (
              <option key={role} value={role}>{ROLE_LABELS[role]}</option>
            ))}
          </select>
        </div>
        <div className="flex flex-col gap-2">
          <label htmlFor="new-password" className="text-lg font-bold">Senha inicial</label>
          <input id="new-password" name="password" type="password" autoComplete="new-password" className={inputClass} />
          <p className="text-base">De {PASSWORD_MIN} a {PASSWORD_MAX} caracteres. Uma frase com espaços funciona bem.</p>
        </div>
      </div>
      {error ? <ErrorNote>{error}</ErrorNote> : null}
      {created ? <InfoNote>{created}</InfoNote> : null}
      <button type="submit" disabled={busy} className={`${buttonPrimary} self-start`}>Criar conta</button>
    </form>
  );
}
