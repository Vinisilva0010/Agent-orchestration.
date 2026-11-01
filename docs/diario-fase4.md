# Diário da Fase 4

Registro do trabalho feito sem acompanhamento. Cada item diz o que foi feito,
as decisões técnicas e o motivo, o que ficou pendente e os riscos encontrados.

## 4.2a — Cadastro de agentes

**Situação encontrada.** O código da 4.2a (migração `agents`, rotas
`/api/agents` e testes) já estava num commit chamado "update" e já tinha sido
enviado ao GitHub, com o CI verde. A alteração no `.gitignore`
(`.claude/settings.local.json`) não é minha e ficou fora dos commits.

**Verificações rodadas:** `cargo fmt --check`, `cargo clippy -D warnings`,
todos os testes do workspace e `scripts/attack-suite.sh` (66 testes nomeados
passaram; `server/tests/agents.rs`: 5 passaram). No `web/`: lint sem avisos,
5 testes e build passaram.

**O que acrescentei.** O teste de papéis do banco
(`each_service_role_gets_only_what_its_code_uses`) só checava `ao_gateway` e
`ao_engine`. Os testes HTTP do server usam a conexão do dono do banco, então
as permissões de `ao_server` nunca eram testadas. Agora o teste também confere
`ao_server`: pode ler agentes, gastos e limites e mudar só as colunas
permitidas de `agents`; não pode mudar o id ou o criador de um agente, apagar
agentes, mudar gastos ou limites, mudar runs, ler chaves ou tokens, nem apagar
auditoria. Também confere que `ao_engine` só lê `agents`.

**Riscos.** Nenhum novo.

## 4.2b — Começar tarefa pela tela

**O que foi feito.**
- Migração `20261101000001_task_starts.sql`: tabela `task_starts` com texto
  da tarefa (1 a 8000 caracteres), orçamento (1 a 100.000.000 micros), quem
  pediu, horários, `run_id` e resultado. Índices para pendentes (geral e por
  pessoa), por agente, um pedido por run, e um índice de runs por agente
  (para a 4.2c).
- Rota `POST /api/agents/{id}/tasks` em `server/src/tasks.rs`.
- No engine: `RunStore::claim_task_start` e o worker passou a processar
  pedidos de tarefa depois dos pedidos sobre runs (decisões, orçamento,
  cancelamento), que são mais urgentes porque alguém está esperando.
- Testes: `server/tests/tasks.rs` (5), `engine/tests/task_starts.rs` (3) e
  novas linhas no teste de papéis.

**Decisões e por quê.**
- *Quem pode começar tarefa:* administrador, ou quem tem permissão `use` ou
  `manage` no agente. Visualizador nunca (403, porque ele enxerga o agente);
  quem não tem permissão recebe 404, como na lista de agentes, para não
  revelar que o agente existe. Agente arquivado recusa com 409.
- *Orçamento:* o padrão é o `task_budget_micros` do agente. Só administrador
  pode pedir um orçamento acima desse padrão (dinheiro é decisão de
  administrador, como já era em `/api/runs/{id}/budget`); membros podem pedir
  menos.
- *Limite de pendentes:* 5 pedidos ainda não iniciados por pessoa. Para dois
  pedidos simultâneos não passarem juntos do limite, a contagem e a inserção
  ficam sob um `pg_advisory_xact_lock` por pessoa (testado com 8 pedidos ao
  mesmo tempo: 5 aceitos, 3 recusados).
- *Privilégio mínimo:* `ao_server` só pode inserir as colunas do pedido
  (não consegue marcar um pedido como processado nem apontar para uma run) e
  ler; `ao_engine` só lê e atualiza `processed_at`, `run_id` e `outcome`.
  Nenhum dos dois apaga.
- *Processado uma vez só:* o worker trava o pedido com
  `FOR UPDATE SKIP LOCKED`, cria a run com o tier do agente e grava o
  `run_id` no pedido na mesma transação; só depois do commit roda o agente.
  Além disso, um gatilho no banco impede mudar um pedido já processado (nem o
  dono do banco consegue reabrir). Testado com 12 workers disputando 6
  pedidos: cada pedido virou exatamente uma run.
- *Auditoria:* `task.requested` (com quem pediu como ator), `task.started` e
  `task.failed`. O texto da tarefa NÃO vai para a auditoria, porque ela nunca
  pode ser apagada e alguém pode colar um dado sensível; vão o SHA-256 e o
  tamanho do texto, que bastam para provar qual texto foi pedido.
- *Se o token de tarefa não sair* (gateway fora do ar), a run é marcada
  `failed` com o erro, em vez de ficar parada como `running`.

**Pendências e riscos.**
- A permissão é checada quando o pedido é gravado, não de novo quando o
  worker começa (o engine não tem acesso às tabelas de contas, de propósito).
  Se a permissão for retirada nos segundos em que o pedido espera na fila, a
  tarefa ainda começa. Registrado no THREAT_MODEL.md.
- Se o worker cair depois de criar a run e antes de rodar o agente, a run
  fica `running` sem ninguém tocando. É o mesmo risco já aceito para os
  pedidos sobre runs; precisa de um "recuperador" no futuro.
- Todos os agentes ainda usam o mesmo conjunto de ferramentas de
  demonstração do engine (`get_time`, `archive_report` e `run_python` quando
  o sandbox está configurado). Ferramentas por agente não fazem parte desta
  fase.

## 4.2c — Acompanhar tarefas

**O que foi feito.**
- `GET /api/agents/{id}/runs`: pedidos ainda na fila (status `queued`)
  primeiro, depois as últimas 50 runs do agente, da mais nova para a mais
  antiga, com quem pediu, status, gasto, orçamento, horários (UTC, formato
  RFC 3339) e o começo do texto da tarefa (300 caracteres).
- `GET /api/runs/{id}`: conversa, atividade, ações esperando aprovação,
  gasto, orçamento, status, resultado final, erro e o que a pessoa pode fazer
  (`can_decide`, `can_cancel`, `can_raise_budget`).
- `GET /api/runs` (lista do que espera decisão) agora também traz nome do
  agente, tarefa, `approvals` já descritas e `can_decide`.
- No engine: o estado da run guarda `approvals`, o histórico de cada chamada
  que esperou uma pessoa (ferramenta, descrição da ferramenta, se é
  irreversível e a decisão).
- Testes: `server/tests/run_views.rs` (4), 3 testes de unidade em
  `server/src/run_views.rs` e 1 novo em `engine/tests/agent.rs`.

**Decisões e por quê.**
- *Como saber se a ação é irreversível:* só o engine conhece as
  ferramentas. Em vez de o server adivinhar pelo nome, o engine grava no
  estado da run, no momento em que para para pedir aprovação, a descrição e
  a marca de irreversível da própria ferramenta. Assim "O que vai acontecer"
  vem de texto escrito pelos desenvolvedores, nunca pelo modelo (o modelo
  poderia escrever "isto é inofensivo"). O texto do modelo aparece separado,
  como "Por que o agente pediu". Runs antigas, sem esse histórico, mostram
  toda ação pendente como irreversível (o lado seguro).
- *Conversa:* só o texto das mensagens; chamadas de ferramenta aparecem pelo
  nome. O raciocínio interno do modelo (blocos `reasoning`) nunca é
  devolvido. Resultados de ferramentas ficam na atividade, cortados em 2000
  caracteres.
- *Atividade:* cada chamada de ferramenta com o resultado: feita, erro,
  recusada, esperando, começada sem resultado conhecido, ou não executada.
  As ações irreversíveis trazem os horários de `engine_tool_executions`.
  A recusa é reconhecida pelo histórico de decisões, não pelo texto do
  resultado.
- *Visibilidade:* a mesma regra da lista de agentes. Quem não pode ver o
  agente recebe 404 (não 403), para não revelar que a run existe. Agente
  arquivado: só administradores veem as runs. Administradores também veem
  runs de agentes que não estão cadastrados (criadas pela linha de comando do
  engine). A lista `GET /api/runs` passou a seguir essa mesma regra (antes
  mostrava runs de agentes arquivados ou não cadastrados para quem tinha
  permissão); o teste antigo de runs ganhou o cadastro do agente `support`.
- *Dependências:* o server ganhou `protocol` e `schemars` só como
  dependências de teste; as duas já estavam no workspace (nenhuma versão
  nova no Cargo.lock).

**Pendências e riscos.**
- As rotas de ação (`/decision`, `/cancel`) continuam respondendo 403 (não
  404) para quem não tem permissão, como antes. Os ids de run são aleatórios
  de 128 bits, então isso não permite descobrir runs; mantive para não mudar
  o comportamento já testado.
