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

### Correção encontrada na verificação ao vivo (entre 4.2c e 4.3)

Subindo a pilha inteira com o provedor mock, a página de uma tarefa deu erro
500: o papel `ao_server` não tinha permissão de leitura em
`engine_tool_executions`. Os testes não pegaram porque o app, nos testes HTTP
do server, usava a conexão do dono do banco (que pode tudo).
- Nova migração `20261101000002_run_views.sql` com só `GRANT SELECT`.
- Todos os testes HTTP do server agora rodam o app com o papel `ao_server`
  (`server/tests/common/mod.rs`, `server_pool`). Conferi que, sem a migração,
  3 testes falham; com ela, passam. Os testes antigos (contas, admin, runs,
  chaves) continuaram passando com o papel real, então não havia outra falta.
- `scripts/e2e-stack.sh` sobe gateway (mock), server, worker e web num banco
  descartável `ao_e2e`, nas portas 8181, 8190 e 3100, sem tocar no banco de
  desenvolvimento nem no servidor web da porta 3000. Cada processo recebe só
  as variáveis dele (`env -i`), então engine e server nunca veem a chave
  mestra nem chaves de provedor. A conta de teste dele só existe nesse banco.

## 4.3 — Telas com dados reais

**O que foi feito.**
- Menu lateral: Início, Agentes, Aprovações (com o número de ações que a
  pessoa pode decidir). Gastos, Pessoas e Configurações entram na 4.4,
  junto com as páginas, para o menu nunca apontar para página que não existe.
- Início: o que espera decisão, caixa para começar tarefa (com escolha do
  agente) e os agentes com status.
- Agentes e página do agente: nome, status, descrição, gasto do mês, caixa de
  tarefa, lista de tarefas (fila e runs) e a tarefa escolhida com gasto,
  conversa, "O que o agente fez" e resultado.
- Aprovações: cartão com "Ação sem volta" (vermelho #FEE2E2/#7F1D1D), "O que
  vai acontecer", "Por que o agente pediu", "Detalhes técnicos" recolhido
  (nome da ferramenta e entrada em JSON), Aprovar, Recusar e o aviso de
  auditoria.
- Atualização a cada 5 segundos enquanto a aba está visível
  (`src/lib/use-poll.ts`), sempre com `cache: "no-store"`.
- Testes vitest: dinheiro em pt-BR (`US$ 3,12` com Intl), leitura de valores
  digitados, porcentagem, horários, status, contagem de decisões, escolha da
  tarefa mostrada e filas (20 testes no total do web).

**Decisões e por quê.**
- *Uma decisão por tarefa:* o server recusa toda ação pendente que não for
  aprovada. Então "Aprovar" aprova todas as ações daquela tarefa e
  "Recusar" recusa todas; quando há mais de uma, o cartão avisa.
- *"O que vai acontecer"* usa a descrição da ferramenta (texto de
  desenvolvedor), em português para as ferramentas conhecidas; o texto do
  modelo fica separado em "Por que o agente pediu", com o aviso de que foi
  escrito pelo agente. Tudo que vem de modelo ou ferramenta é mostrado como
  texto (sem `dangerouslySetInnerHTML`, sem `style` inline).
- *Valores pequenos:* gastos abaixo de 1 centavo aparecem como
  "menos de US$ 0,01", nunca como zero.
- *Valores digitados:* a vírgula é sempre a casa decimal; "3,125" é recusado
  em vez de virar três mil dólares.
- *Erros do server* chegam em inglês; a tela mostra mensagens em português
  por código de erro. O erro técnico de uma run aparece só dentro de
  "Detalhes técnicos do problema".
- *Negrito:* o corpo da página passou a ser negrito por padrão (regra de
  texto grande e em negrito); isso também deixa a tela de login toda em
  negrito.
- *Barras de progresso* em SVG com atributos `width`/`height`.

**Verificação ao vivo.** Pilha e2e com provedor mock: tarefa com
`[mock:tool:archive_report]` parou esperando aprovação; tarefa com
`[mock:tool:get_time]` concluiu. Capturas de tela da Início, do agente, de
Aprovações e de Aprovações em largura de celular, sem nenhum erro de console
(a CSP não bloqueou nada). O Chromium do Playwright não roda direto nesta
máquina (faltam bibliotecas do sistema, libnss3 e libnspr4, que exigiriam
sudo); usei a imagem Docker oficial do Playwright 1.64.1, fixada por digest.

**Pendências.**
- Não há tela para criar agentes; hoje isso é feito pela API
  (`POST /api/agents`). Não estava na lista.
- O Dependabot abriu uma atualização do `minimatch` no web cujo CI falhou.
  Não mexi (regra: não fazer merge de PRs de terceiros).

## 4.4 — Gastos, Pessoas e Configurações

**O que foi feito.**
- Menu completo: Gastos para todos; Pessoas e Configurações só aparecem
  para administradores (e as páginas dizem "só administradores" a quem
  chegar pelo endereço; o server recusa de qualquer forma).
- Gastos: total da empresa no mês contra o limite (`/api/usage`, só
  administrador), "Parado por orçamento" com campo de novo limite, "Liberar
  e continuar" (`/api/runs/{id}/budget`) e "Cancelar a tarefa"
  (`/api/runs/{id}/cancel`, com confirmação), e a tabela por agente com
  "Mudar limite" (`PUT /api/agents/{id}/budget`).
- Pessoas: criar conta, mudar papel, desativar (com confirmação) e
  reativar, permissões por agente (Sem acesso / Pode usar / Pode usar e
  gerenciar).
- Configurações: mostra só se cada provedor tem chave e quando foi trocada;
  campo de senha para cadastrar ou trocar; remover com confirmação.
- Server: nova rota `GET /api/users/{id}/agents` (só administrador) para a
  tela saber as permissões atuais, com teste
  (`only_administrators_read_permissions`); `GET /api/runs` passou a dizer
  `can_cancel`.

**Decisões e por quê.**
- *Chave de provedor:* a chave é lida do formulário só na hora do envio,
  nunca fica no estado do React, e o campo é limpo depois, dando certo ou
  não. A tela nunca recebe a chave de volta (o server nem tem como).
- *Senha inicial de conta nova:* mesmo cuidado (lida do formulário, campo
  limpo depois).
- *Liberar orçamento:* a tela só aceita valor maior que o atual e até
  US$ 100 por tarefa; quem não é administrador vê o aviso de que só um
  administrador pode liberar, e pode cancelar se gerencia o agente.
- *Desativar a própria conta:* o botão não aparece na própria linha, para
  ninguém se trancar fora sem querer (o server já impede desativar o último
  administrador).
- *Nada de texto cinza claro:* tirei o placeholder do campo de valor (era
  cinza sobre branco) e pus o exemplo no texto de ajuda.

**Verificação ao vivo.** Pilha e2e (mock): tarefa criada com orçamento de 1
micro-dólar parou em "Parado por orçamento"; as três telas abriram com dados
reais e sem erros de console.

**Pendências.** Não testei salvar uma chave de verdade (nem falsa) na tela
ao vivo; o caminho da chave já é coberto pelos testes do server
(`keys_go_to_the_gateway_with_the_admin_secret_and_never_come_back`).
