"use client";

import { PROVIDERS, ProviderKey } from "@/components/provider-key";
import { useMe } from "@/components/shell";
import { ErrorNote, PageTitle } from "@/components/ui";
import { messageFor } from "@/lib/api";
import type { Credential } from "@/lib/types";
import { usePoll } from "@/lib/use-poll";

export default function SettingsPage() {
  const me = useMe();
  const isAdmin = me?.role === "admin";
  const credentials = usePoll<Credential[]>(isAdmin ? "/api/credentials" : null, 30_000);

  if (me && !isAdmin) {
    return (
      <>
        <PageTitle>Configurações</PageTitle>
        <p className="text-lg">Só administradores podem mudar as configurações.</p>
      </>
    );
  }

  return (
    <>
      <PageTitle subtitle="Chaves das empresas de IA que os agentes usam.">Configurações</PageTitle>
      <p className="text-lg">
        As chaves ficam guardadas cifradas num serviço separado. Nenhuma tela, agente ou registro mostra a chave.
      </p>
      {credentials.error ? <ErrorNote>{messageFor(credentials.error)}</ErrorNote> : null}
      {credentials.loading ? <p className="text-lg">Carregando...</p> : null}
      {credentials.data ? (
        <div className="flex flex-col gap-6">
          {PROVIDERS.map((provider) => (
            <ProviderKey key={provider.id} id={provider.id} name={provider.name}
              credential={credentials.data?.find((item) => item.provider === provider.id)}
              onChanged={credentials.refresh} />
          ))}
        </div>
      ) : null}
    </>
  );
}
