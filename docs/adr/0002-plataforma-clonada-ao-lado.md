# ADR 0002: Bibliotecas e módulos da plataforma pelo clone ao lado

- **Situação:** aceita
- **Data:** 2026-11-15

## Contexto

Os crates (`saci-problem`, `saci-authz`, `saci-events`...) e os módulos Terraform (`rust-lambda`, `dynamodb-table`...) vêm do `saci-boiuna`. O `templates/scaffold.sh` da plataforma usa dependências git com a tag `v0.1.0`, mas o `saci-boiuna` ainda não publicou tags.

## Decisão

Enquanto não houver tag, o `saci-commerce` usa **caminhos relativos** para o `saci-boiuna` clonado ao lado (`../saci-boiuna`): no `Cargo.toml` do workspace e nos `source` dos módulos Terraform. O CI faz os dois checkouts lado a lado.

## Consequências

- Funciona igual na máquina, no CI e no LocalStack, sem rede para um repositório privado.
- **Pendência na plataforma:** o `reusable-deploy-service.yml` clona o `saci-boiuna` em `.platform/`, não ao lado; até ele aceitar o checkout ao lado (ou a plataforma publicar `v0.1.0` e este repositório trocar para git), a implantação por CI fica bloqueada. A troca para git é mecânica: as mesmas substituições do `scaffold.sh`.
