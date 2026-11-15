# Primeira implantação num ambiente

Pré-requisito: a fundação do `saci-boiuna` aplicada no ambiente (contas, rede, borda com os baldes `cdn` e `files`, API Gateway e autorizador, barramento), como no runbook `bootstrap-aws.md` de lá.

## 1. Papéis de implantação (no saci-boiuna)

Registre cada domínio como "serviço" em `ci/aws/stacks/github-oidc/envs/<conta>.tfvars` do `saci-boiuna` (ADR 0003):

```hcl
services = {
  sellers       = { repository = "saci-commerce", github_environment = "sellers-dev" }
  catalog       = { repository = "saci-commerce", github_environment = "catalog-dev" }
  media         = { repository = "saci-commerce", github_environment = "media-dev" }
  shipping      = { repository = "saci-commerce", github_environment = "shipping-dev" }
  orders        = { repository = "saci-commerce", github_environment = "orders-dev" }
  checkout      = { repository = "saci-commerce", github_environment = "checkout-dev" }
  customers     = { repository = "saci-commerce", github_environment = "customers-dev" }
  reviews       = { repository = "saci-commerce", github_environment = "reviews-dev" }
  support       = { repository = "saci-commerce", github_environment = "support-dev" }
  notifications = { repository = "saci-commerce", github_environment = "notifications-dev" }
}
```

E o publicador de layouts em `static_publishers` (balde `cdn`, prefixo `layouts/`). Crie no GitHub os ambientes `<domínio>-dev`, `<domínio>-prod` (com revisores) e `layouts-<env>`, com as variáveis `AWS_DEPLOY_ROLE_ARN`, `TF_STATE_BUCKET` e `AWS_PUBLISH_ROLE_ARN`.

## 2. Stack compartilhado (uma vez, com um papel de administração)

```bash
cd infra/aws/stacks/shared
terraform init -backend-config=envs/<env>.s3.tfbackend -backend-config="bucket=<estado>"
terraform apply -var-file=envs/<env>.tfvars
```

Cria a chave dos cursores, o segredo dos webhooks da transportadora e, fora de produção, a tabela do sandbox.

## 3. Valores por ambiente

Preencha `<ACCOUNT_ID>` em `domains/*/infra/aws/envs/<env>.tfvars` e `.s3.tfbackend`. Em produção:

- `use_sandbox = false` e `external_functions` com as funções internas do `saci-identity`, `saci-payments`, `saci-finance` e `saci-assistant` (sem elas, a produção não sobe: ADR 0008);
- `checkout`: `trusted_issuers` (o emissor do Cognito) e `guest_issuer`;
- `shipping`: `files_key_pair_id` e `files_private_key_param` (o par de chaves do grupo de `files.`).

## 4. Implantação

Pelo workflow `deploy.yml` (push na `main` → dev; aprovação → prod). A ordem segue as migrações: `sellers → catalog → media → shipping → orders → checkout → customers → reviews → support → notifications`. Cada stack invoca a função `migrate` do domínio no fim; em dev e staging, com as sementes de exemplo.

## 5. Guest token na plataforma

Depois do primeiro apply do `checkout`, pegue a saída `guest_public_key_pem`, converta para JWKS (ES256, `kid` = `guest-<env>-1`) e cadastre o emissor em `trusted_issuers` do stack `gateway/aws/stacks/api` do `saci-boiuna`:

```hcl
"https://checkout.saci.art.br" = { kind = "guest", audiences = ["saci-api"], jwks_json = "<JWKS>", max_ttl_secs = 2592000 }
```

## 6. Conferência

```bash
curl https://api-dev.saci.art.br/v1/catalog/categories
curl https://api-dev.saci.art.br/v1/sellers/storefronts/barro-vivo
```

E o roteiro `local-env/scripts/smoke.sh` adaptado à URL do ambiente.
