import { describe, expect, it, vi } from "vitest";
import { ApiError, api, messageFor } from "./api";

function reply(status: number, body?: unknown) {
  return vi.fn(async () =>
    body === undefined ? new Response(null, { status }) : Response.json(body, { status }),
  );
}

describe("api", () => {
  it("sends same-origin JSON requests that are never cached", async () => {
    const fetcher = reply(200, { ok: true });
    await api("/api/runs", { method: "POST", body: { a: 1 }, fetcher });
    const [path, init] = fetcher.mock.calls[0] as unknown as [string, RequestInit];
    expect(path).toBe("/api/runs");
    expect(init.credentials).toBe("same-origin");
    expect(init.cache).toBe("no-store");
    expect(init.body).toBe('{"a":1}');
  });

  it("refuses anything but the server's /api paths", async () => {
    await expect(api("https://evil.example/api/me")).rejects.toThrow();
  });

  it("sends an ended session to sign-in, but not a wrong password", async () => {
    const ended = vi.fn();
    await expect(api("/api/me", { fetcher: reply(401, {}), onUnauthorized: ended })).rejects.toBeInstanceOf(ApiError);
    expect(ended).toHaveBeenCalledOnce();

    const wrongPassword = vi.fn();
    const error = await api("/api/session", {
      method: "POST",
      body: {},
      fetcher: reply(401, { error: { code: "invalid_credentials", message: "x" } }),
      onUnauthorized: wrongPassword,
    }).catch((e: unknown) => e);
    expect(wrongPassword).not.toHaveBeenCalled();
    expect(messageFor(error)).toBe("E-mail ou senha incorretos.");
  });

  it("returns nothing for 204 and a plain message for unknown errors", async () => {
    await expect(api("/api/session", { method: "DELETE", fetcher: reply(204) })).resolves.toBeUndefined();
    const error = await api("/api/usage", { fetcher: reply(500, {}) }).catch((e: unknown) => e);
    expect(messageFor(error)).toBe("Não foi possível concluir. Tente de novo em instantes.");
  });
});
