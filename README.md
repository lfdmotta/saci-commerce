# saci-commerce

Os serviços públicos da **Saci**, o marketplace que conecta artesãos a quem compra: a vitrine aberta (catálogo, busca, vitrines, avaliações, frete), a compra (checkout, pedidos) e o painel do artesão (anúncios, imagens, vendas, equipe, atendimento). 

| | |
| --- | --- |
| **Tipo** | Domínio ([`repo.yaml`](repo.yaml)) |
| **Contas AWS** | Prod, Staging, Dev |
| **Classe máxima de dado** | C2, com só referências ao C3 (o cofre de dados pessoais é do `saci-identity`, D9) |
| **Linguagens** | Rust (serviços), Terraform (infraestrutura) |
| **Plataforma** | [`saci-boiuna`](../saci-boiuna) (clonado ao lado: bibliotecas, módulos, API Gateway, autorizador, barramento) |
| **Contratos** | [`saci-contracts`](../saci-contracts) (OpenAPI e CloudEvents) |

## Domínios e rotas

| Domínio | Raiz | Serviços | Contrato |
| --- | --- | --- | --- |
| `catalog` | `/v1/catalog` | [`catalog-api`](domains/catalog/catalog-api), [`search-indexer`](domains/catalog/search-indexer) | `saci-contracts/http/catalog.yaml` |
| `sellers` | `/v1/sellers` | [`sellers-api`](domains/sellers/sellers-api) | `http/sellers.yaml` |
| `customers` | `/v1/customers` | [`customers-api`](domains/customers/customers-api) | `http/customers.yaml` |
| `reviews` | `/v1/reviews` | [`reviews-api`](domains/reviews/reviews-api) | `http/reviews.yaml` |
| `media` | `/v1/media` | [`media-api`](domains/media/media-api), [`media-pipeline`](domains/media/media-pipeline) | proposta 5.5 |
| `shipping` | `/v1/shipping` | [`shipping-api`](domains/shipping/shipping-api), [`carrier-webhooks`](domains/shipping/carrier-webhooks) | `http/shipping.yaml` |
| `checkout` | `/v1/checkout` | [`checkout-saga`](domains/checkout/checkout-saga) | proposta 3.3 e 5.7 |
| `orders` | `/v1/orders` | [`orders-api`](domains/orders/orders-api) | proposta 5.8 |
| `support` | `/v1/support` | [`support-api`](domains/support/support-api) | proposta 5.11 |
| `notifications` | `/v1/notifications` | [`notifications-api`](domains/notifications/notifications-api), [`dispatcher`](domains/notifications/dispatcher) | proposta 9.6 |
| `content` | `cdn.saci.art.br/layouts` | [`content-publisher`](domains/content/content-publisher), [`layouts`](domains/content/layouts) | `saci-contracts/layouts` |

São **62 rotas** no API Gateway: todas as que a interface já chama nestes domínios e as que a proposta e os contratos preveem para as próximas telas (as de `identity`, `payments`, `finance` e `assistant` são de outros repositórios). A lista completa, com o nível de acesso de cada uma, está nos `service.yaml` e nos stacks `domains/<domínio>/infra/aws/main.tf`.

## Estrutura

```
saci-commerce/
├── crates/
│   ├── kit/            # comum: HTTP sem framework, porta de armazenamento, eventos, cursores, idempotência (AWS só na feature `aws`)
│   └── sandbox/        # identity, kyc, payments, finance e assistant simulados — só fora de produção
├── domains/<domínio>/
│   ├── <serviço>/      # src/ (núcleo sem nuvem), migrations/, service.yaml, Taskfile.yml
│   ├── lambda/         # o binário implantável do domínio (saci-<domínio>), uma função por papel
│   └── infra/aws/      # o stack Terraform do domínio
├── infra/aws/
│   ├── modules/domain  # o que todo domínio implanta: chave, tabelas, funções, rotas, eventos, migração
│   ├── modules/routes  # /v1/<domínio> no API Gateway da plataforma, uma função por rota
│   └── stacks/shared   # chave dos cursores, segredo dos webhooks, tabela do sandbox
├── local-env/          # servidor de desenvolvimento, LocalStack, roteiro ponta a ponta
└── docs/               # arquitetura, ADRs, contratos internos, runbooks
```

O repositório é organizado por **domínio**, não por linguagem nem por nuvem. O código de cada serviço não conhece a AWS: as portas (armazenamento, arquivos, eventos, chamadas internas, assinatura de tokens) têm adaptador em memória e adaptador AWS, e trocar de nuvem é escrever outro adaptador e outro `infra/<nuvem>` ([ADR 0001](docs/adr/0001-estrutura-e-portas.md)).

## Começo rápido

Precisa do [Rust](https://rustup.rs) (versão fixa em `rust-toolchain.toml`), do Node 22+ e do `saci-boiuna` clonado ao lado.

```bash
task dev                         # todos os serviços em http://localhost:8787, com dados de exemplo
task dev:e2e                     # 82 verificações pelos caminhos da interface (em outro terminal)
task lint test tf:validate       # qualidade
```

A interface já aponta para este servidor sem configuração (`npm run web` lá); [`local-env/interface.env.example`](local-env/interface.env.example) mostra o que dá para mudar. Tudo junto (identity, commerce, interface e LocalStack): `saci-sume/ambiente-local.md`. Contas de exemplo, sandbox de pagamento e o LocalStack: [`local-env/README.md`](local-env/README.md).

## Documentação

- [Arquitetura](docs/architecture.md): serviços, funções, dados, eventos, fluxo de uma compra, segurança aplicada.
- [Contratos internos](docs/contratos-internos.md): as operações síncronas entre domínios e o que este repositório espera do `saci-identity`, do `saci-payments`, do `saci-finance` e do `saci-assistant`.
- [Decisões (ADRs)](docs/adr/).
- [Runbooks](docs/runbooks/): primeira implantação, operação, sagas e filas.

Commits seguem os prefixos de `saci-sume/commits.md` (`ENH`, `FIX`, `IAC`, `DOC`...) com o número da issue no título.
