/**
 * The browser's only way to the server: same-origin requests with the
 * HttpOnly session cookie. The cookie is never read or stored by this code.
 */

export type Role = "admin" | "member" | "viewer";

export type Me = {
  id: number;
  email: string;
  display_name: string;
  role: Role;
};

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

type Options = {
  method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  body?: unknown;
  fetcher?: typeof fetch;
  onUnauthorized?: () => void;
};

export function goToSignIn() {
  if (typeof window !== "undefined") {
    window.location.replace(new URL("/login", window.location.href).href);
  }
}

export async function api<T = void>(path: string, options: Options = {}): Promise<T> {
  if (!path.startsWith("/api/")) {
    throw new Error("api paths start with /api/");
  }
  const fetcher = options.fetcher ?? fetch;
  const hasBody = options.body !== undefined;
  const response = await fetcher(path, {
    method: options.method ?? "GET",
    credentials: "same-origin",
    cache: "no-store",
    headers: hasBody ? { "Content-Type": "application/json" } : undefined,
    body: hasBody ? JSON.stringify(options.body) : undefined,
  });
  // A wrong password on sign-in is also 401; only an ended session sends the
  // person back to the sign-in page.
  if (response.status === 401 && path !== "/api/session") {
    (options.onUnauthorized ?? goToSignIn)();
  }
  if (!response.ok) {
    const body = (await response.json().catch(() => ({}))) as {
      error?: { code?: string; message?: string };
    };
    throw new ApiError(
      response.status,
      body.error?.code ?? "unknown",
      body.error?.message ?? `request failed (${response.status})`,
    );
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

const MESSAGES: Record<string, string> = {
  invalid_credentials: "E-mail ou senha incorretos.",
  too_many_attempts: "Muitas tentativas seguidas. Espere alguns minutos e tente de novo.",
  forbidden: "Você não tem permissão para fazer isso.",
  forbidden_origin: "O pedido não veio desta página. Recarregue e tente de novo.",
  unauthorized: "Sua sessão terminou. Entre de novo.",
  not_found: "Não encontrado. Pode ter sido removido, ou você não tem acesso.",
  request_pending: "Já existe um pedido para esta tarefa sendo feito. Espere alguns segundos.",
  not_waiting: "Esta tarefa não está mais esperando. A página foi atualizada.",
  agent_archived: "Este agente está arquivado e não recebe tarefas novas.",
  too_many_pending_tasks: "Você já tem 5 tarefas esperando para começar. Espere uma delas começar.",
  agent_exists: "Já existe um agente com este identificador.",
  email_taken: "Já existe uma conta com este e-mail.",
  last_admin: "Não é possível: este é o último administrador ativo.",
  invalid_request: "Confira os dados digitados e tente de novo.",
  gateway_unavailable: "O serviço de modelos não respondeu. Tente de novo em instantes.",
};


/** What to tell the person, in plain words. */
export function messageFor(error: unknown): string {
  if (error instanceof ApiError && error.code in MESSAGES) {
    return MESSAGES[error.code];
  }
  return "Não foi possível concluir. Tente de novo em instantes.";
}

export const ROLE_LABELS: Record<Role, string> = {
  admin: "Administrador",
  member: "Membro",
  viewer: "Somente leitura",
};
