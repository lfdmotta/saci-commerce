## O que muda

<!-- Uma ou duas frases: qual domínio e por quê. Título com prefixo e issue (DOC, ENH, FIX, IAC... #123). -->

## Impacto

- [ ] Muda contrato HTTP ou evento (mudou antes em `saci-contracts`?)
- [ ] Muda classe de dado, IAM, KMS ou exposição (revisão de segurança)
- [ ] Muda tabela ou formato de documento (migração em `src/seed.rs`, só para frente)
- [ ] Precisa de ADR nova (`docs/adr/`)

## Como testei

- [ ] `task lint test tf:validate`
- [ ] `task dev:e2e` (servidor de desenvolvimento + roteiro da interface)
- [ ] `task local:deploy local:smoke` (se mexeu em `infra/` ou nos binários)

## Dados

- [ ] Nenhum dado real (CPF, contato, endereço) em código, testes ou sementes
