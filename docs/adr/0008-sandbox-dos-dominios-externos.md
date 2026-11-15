# ADR 0008: Sandbox dos domínios de outros repositórios

- **Situação:** aceita
- **Data:** 2026-11-15

## Contexto

O `saci-commerce` depende de `identity` (cofre, sessões), `kyc`, `payments`, `finance` e `assistant`, que vivem em repositórios ainda não criados. A diretiva pede o teste local completo da interface ligada ao back-end.

## Decisão

- `crates/sandbox` implementa esses domínios com regras fixas e dados fictícios: as operações internas e, para a interface, as rotas HTTP de `/v1/identity` e `/v1/payments`.
- O sandbox **recusa produção** em dois lugares: na criação (`SACI_ENVIRONMENT=prod`) e na validação do Terraform (`use_sandbox` proibido em `prod`).
- Fora de produção, o estado do sandbox fica numa tabela própria (`saci-<env>-sandbox`), dividida por todas as funções.

## Consequências

- Dev e staging funcionam ponta a ponta antes dos outros repositórios.
- Cada repositório real substitui o sandbox implementando as mesmas operações ([contratos-internos.md](../contratos-internos.md)).
