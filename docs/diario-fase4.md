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
