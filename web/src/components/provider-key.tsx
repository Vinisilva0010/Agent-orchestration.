"use client";

import { useState, type FormEvent } from "react";
import { api, messageFor } from "@/lib/api";
import { formatTime } from "@/lib/format";
import { keyProblem } from "@/lib/keys";
import type { Credential } from "@/lib/types";
import { Badge, Card, ErrorNote, InfoNote, buttonDanger, buttonPrimary, inputClass } from "./ui";

export const PROVIDERS = [
  { id: "anthropic", name: "Anthropic (modelos Claude)" },
  { id: "openai", name: "OpenAI (modelos GPT)" },
  { id: "gemini", name: "Google (modelos Gemini)" },
] as const;

/**
 * One provider's key. The page only ever knows whether a key exists. The key
 * typed here is read from the form when sending, sent once and the field is
 * cleared; it is never kept in the page's state or shown back.
 */
export function ProviderKey({ id, name, credential, onChanged }: {
  id: string;
  name: string;
  credential: Credential | undefined;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const fieldId = `key-${id}`;

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = event.currentTarget;
    const field = form.elements.namedItem("api_key") as HTMLInputElement | null;
    const key = field?.value.trim() ?? "";
    setDone(null);
    const problem = keyProblem(key);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api(`/api/credentials/${id}`, { method: "PUT", body: { api_key: key } });
      setDone("Chave salva. Ela fica guardada de forma cifrada e não aparece mais nesta tela.");
      onChanged();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      // Cleared whether or not it worked: the key never stays on the page.
      form.reset();
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    setError(null);
    setDone(null);
    try {
      await api(`/api/credentials/${id}`, { method: "DELETE" });
      setDone("Chave removida. Os agentes não usam mais este provedor.");
      onChanged();
    } catch (failure) {
      setError(messageFor(failure));
    } finally {
      setBusy(false);
      setConfirmRemove(false);
    }
  }

  return (
    <Card>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h3 className="text-2xl font-bold">{name}</h3>
        <Badge tone={credential ? "done" : "neutral"}>{credential ? "Chave cadastrada" : "Sem chave"}</Badge>
      </div>
      {credential ? (
        <p className="text-lg">Última troca: {formatTime(new Date(credential.updated_at_unix * 1000).toISOString())}.</p>
      ) : null}
      <form onSubmit={save} className="flex flex-col gap-3" noValidate autoComplete="off">
        <label htmlFor={fieldId} className="text-lg font-bold">{credential ? "Trocar a chave" : "Cadastrar a chave"}</label>
        <input id={fieldId} name="api_key" type="password" autoComplete="off" spellCheck={false}
          className={`${inputClass} max-w-xl`} />
        <p className="text-base">A chave é enviada uma única vez e nunca é mostrada de volta.</p>
        <button type="submit" disabled={busy} className={`${buttonPrimary} self-start`}>Salvar chave</button>
      </form>
      {credential ? (
        confirmRemove ? (
          <div className="flex flex-wrap items-center gap-3">
            <span className="text-lg">Remover a chave? Tarefas que usam este provedor vão parar de funcionar.</span>
            <button type="button" disabled={busy} onClick={remove} className={buttonDanger}>Sim, remover</button>
            <button type="button" onClick={() => setConfirmRemove(false)} className="text-lg font-bold underline">Não</button>
          </div>
        ) : (
          <button type="button" disabled={busy} onClick={() => setConfirmRemove(true)} className={`${buttonDanger} self-start`}>
            Remover chave
          </button>
        )
      ) : null}
      {error ? <ErrorNote>{error}</ErrorNote> : null}
      {done ? <InfoNote>{done}</InfoNote> : null}
    </Card>
  );
}
