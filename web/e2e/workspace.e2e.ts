import { expect, test, type Page } from "@playwright/test";

// The account created by scripts/e2e-stack.sh in its throwaway database.
const ADMIN_EMAIL = "admin@e2e.test";
const ADMIN_PASSWORD = "e2e administrator passphrase";
// The gateway runs its mock provider: "[mock:tool:NAME]" in a task makes the
// model ask for that tool; anything else gets a plain text answer.
const ARCHIVE = "[mock:tool:archive_report]";

const agentId = `e2e-${Date.now().toString(36)}`;
const agentName = `Agente de teste ${agentId}`;

/** Fails the test on any browser error, such as a CSP violation. */
function watchConsole(page: Page): string[] {
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(error.message));
  return problems;
}

async function signIn(page: Page) {
  await page.goto("/login");
  await page.getByLabel("E-mail").fill(ADMIN_EMAIL);
  await page.getByLabel("Senha").fill(ADMIN_PASSWORD);
  await page.getByRole("button", { name: "Entrar" }).click();
  await expect(page).toHaveURL("/");
  await expect(page.getByText("Olá, Administradora.")).toBeVisible();
}

/** State-changing API calls need the workspace origin, as the browser sends. */
async function apiPost(page: Page, path: string, data: unknown) {
  const origin = new URL(page.url()).origin;
  const response = await page.request.post(path, { data, headers: { Origin: origin } });
  expect(response.ok(), `${path}: ${response.status()}`).toBeTruthy();
  return response;
}

async function sendTask(page: Page, text: string) {
  await page.goto(`/agentes/${agentId}`);
  await page.getByLabel("O que o agente deve fazer?").fill(text);
  await page.getByRole("button", { name: "Enviar tarefa" }).click();
  await expect(page.getByText("Tarefa enviada.")).toBeVisible();
}

/** The approval card of the run whose task contains this text. */
function approvalCard(page: Page, task: string) {
  return page.getByRole("listitem").filter({ hasText: task });
}

test.describe.serial("workspace with the mock provider", () => {
  test.beforeAll(async ({ browser }) => {
    const page = await browser.newPage();
    await signIn(page);
    await apiPost(page, "/api/agents", {
      id: agentId,
      display_name: agentName,
      description: "Agente criado pelos testes ponta a ponta.",
      tier: "small",
      task_budget_micros: 2_000_000,
    });
    await page.close();
  });

  test("signs in and sees the agent", async ({ page }) => {
    const problems = watchConsole(page);
    await signIn(page);
    await expect(page.getByRole("heading", { name: agentName })).toBeVisible();
    expect(problems).toEqual([]);
  });

  test("starts a task, approves the irreversible action and the task finishes", async ({ page }) => {
    const problems = watchConsole(page);
    await signIn(page);
    const task = `Arquive o relatório A ${ARCHIVE}`;
    await sendTask(page, task);

    await page.getByRole("link", { name: /^Aprovações/ }).click();
    const card = approvalCard(page, "Arquive o relatório A");
    await expect(card.getByText("Ação sem volta")).toBeVisible();
    await expect(card.getByText("O que vai acontecer")).toBeVisible();
    await expect(card.getByText("Arquiva um relatório. Isto não pode ser desfeito.")).toBeVisible();
    await card.getByText("Detalhes técnicos").click();
    await expect(card.getByText("archive_report", { exact: true })).toBeVisible();
    await expect(card.getByText("Sua decisão fica guardada no registro de auditoria")).toBeVisible();
    await card.getByRole("button", { name: "Aprovar" }).click();
    await expect(card).toHaveCount(0); // processed: the card leaves the list

    await page.goto(`/agentes/${agentId}`);
    await page.getByRole("button", { name: /Arquive o relatório A/ }).click();
    const activity = page.getByRole("region", { name: "O que o agente fez" });
    await expect(activity.getByText("Feito")).toBeVisible();
    await expect(activity.getByText("Arquivar um relatório")).toBeVisible();
    await expect(page.getByRole("button", { name: /Concluída[\s\S]*Arquive o relatório A/ })).toBeVisible();
    expect(problems).toEqual([]);
  });

  test("rejecting an action means it never runs", async ({ page }) => {
    const problems = watchConsole(page);
    await signIn(page);
    await sendTask(page, `Arquive o relatório B ${ARCHIVE}`);
    await page.goto("/aprovacoes");
    const card = approvalCard(page, "Arquive o relatório B");
    await card.getByRole("button", { name: "Recusar" }).click();
    await expect(card).toHaveCount(0); // processed: the card leaves the list

    await page.goto(`/agentes/${agentId}`);
    await page.getByRole("button", { name: /Arquive o relatório B/ }).click();
    const activity = page.getByRole("region", { name: "O que o agente fez" });
    await expect(activity.getByText("Recusado por uma pessoa")).toBeVisible();
    await expect(activity.getByText("Feito")).toHaveCount(0);
    expect(problems).toEqual([]);
  });

  test("a task stopped by its budget goes on after an administrator raises it", async ({ page }) => {
    const problems = watchConsole(page);
    await signIn(page);
    // One millionth of a dollar cannot pay for any model call.
    await apiPost(page, `/api/agents/${agentId}/tasks`, { task: "Resuma o relatório C", budget_micros: 1 });

    await page.getByRole("link", { name: "Gastos" }).click();
    const card = page.getByRole("listitem").filter({ hasText: "Resuma o relatório C" });
    await expect(card.getByText("Parado por orçamento")).toBeVisible();
    await card.getByLabel("Novo limite desta tarefa (em US$)").fill("0,50");
    await card.getByRole("button", { name: "Liberar e continuar" }).click();
    await expect(card).toHaveCount(0); // processed: the card leaves the list

    await page.goto(`/agentes/${agentId}`);
    await expect(page.getByRole("button", { name: /Concluída[\s\S]*Resuma o relatório C/ })).toBeVisible();
    expect(problems).toEqual([]);
  });

  test("signs out and the pages need signing in again", async ({ page }) => {
    await signIn(page);
    await page.getByRole("button", { name: "Sair" }).click();
    await expect(page).toHaveURL(/\/login$/);
    await page.goto("/aprovacoes");
    await expect(page).toHaveURL(/\/login$/);
    const response = await page.request.get("/api/me");
    expect(response.status()).toBe(401);
  });
});
