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
