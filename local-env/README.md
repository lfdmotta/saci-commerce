# Ambiente local

Dois jeitos de rodar o `saci-commerce` na máquina, com **o mesmo código Rust** das Lambdas:

| | Servidor de desenvolvimento | LocalStack |
| --- | --- | --- |
| Para | Usar a **interface** contra o back-end real; desenvolver e depurar serviços | Conferir a **infraestrutura**: os mesmos stacks Terraform da nuvem, API Gateway, autorizador da plataforma, filas, streams |
| Precisa de | Rust | Docker, token do LocalStack, o ambiente local do `saci-boiuna` |
| Sobe em | segundos (`task dev`) | minutos (`task local:deploy`) |
| Dados | em memória, sementes a cada início | DynamoDB do LocalStack, sementes pela função `migrate` |
| Identidade e pagamentos | sandbox em processo (`/v1/identity`, `/v1/payments`) | sandbox nas funções (tabela `saci-local-sandbox`); tokens emitidos com `saci-devtools` |

## Interface + back-end local (servidor de desenvolvimento)

```bash
# terminal 1: o back-end (http://localhost:8787)
task dev                                  # ou: cargo run -p saci-commerce-dev

# terminal 2: a interface (sem .env ela já aponta para http://localhost:8787)
cd ../saci-interface && npm run web       # http://localhost:8081
```

A interface simula na própria máquina, por padrão, o `finance` e o `assistant`; o resto vem deste servidor. Com o `saci-finance` no ar (abaixo), use `EXPO_PUBLIC_MOCKS=assistant` na interface. Para mudar isso, ou liberar as fotos das sementes, copie [`interface.env.example`](interface.env.example) para `saci-interface/.env`. O guia de tudo junto (identity, commerce, interface e LocalStack) está em `saci-sume/ambiente-local.md`.

O servidor faz o papel de toda a borda: CORS para o site do Expo, autenticação equivalente ao autorizador da plataforma (JWT ES256, papéis de vitrine lidos das concessões que o `sellers` grava, guest token do checkout), o `cdn.` e o `files.` (envio direto de imagens, fotos publicadas, etiquetas assinadas) e os layouts de `domains/content/layouts`.

### Com o saci-identity de verdade

Com o `saci-identity-dev` no ar (`task dev` no `saci-identity`, porta 8788), suba este servidor com `SACI_DEV_IDENTITY_URL=http://localhost:8788`: `/v1/identity/*` passa a ser repassado a ele (cadastro, sessões, MFA, pedidos do titular), os alvos internos `identity` (cofre) e `kyc` vão para `/internal/{alvo}` dele, e o `Authorization` é conferido com a chave pública das sessões dele. O resto (assistant e, sem as pontes abaixo, payments e finance) continua no sandbox. As contas de exemplo são as mesmas; o perfil passa a vir com e-mail e celular **mascarados**, como o cofre devolve. Detalhes em `saci-identity/local-env/README.md` e `src/identity_bridge.rs`.

### Com o saci-payments de verdade

Com o `saci-payments-dev` no ar (`task dev` no `saci-payments`, porta 8789), suba este servidor também com `SACI_DEV_PAYMENTS_URL=http://localhost:8789`: `/v1/payments/*` (`pay.`) passa a ser repassado a ele com a conta já conferida aqui (`x-saci-dev-account`, o papel do contexto do autorizador), o alvo interno `payments` (`charge`, `charge_status`) vai para `/internal/payments` dele, os eventos `payments.*` chegam ao barramento daqui por `POST /dev/events` e `checkout.order.abandoned.v1` vai para lá. O `client-config` passa a apontar a tokenização para o PSP simulado de lá (`http://localhost:8789/psp/sandbox/tokens`). Detalhes em `saci-payments/local-env/README.md` e `src/payments_bridge.rs`.

### Com o saci-finance de verdade

Com o `saci-finance-dev` no ar (`task dev` no `saci-finance`, porta 8790), suba este servidor também com `SACI_DEV_FINANCE_URL=http://localhost:8790`: `/v1/finance/*` passa a ser repassado a ele com a conta **e o papel na vitrine** conferidos aqui (`x-saci-dev-account`, `x-saci-dev-storefront`, `x-saci-dev-role`: o Financeiro é só da pessoa responsável), o alvo interno `finance` (`sale_payment`, os valores da venda no painel) vai para `/internal/finance` dele, os eventos que ele assina (`payments.*`, `orders.sale.delivered.v1`, `orders.return.opened.v1`, `shipping.label.purchased.v1`) são entregues em `POST /dev/events` de lá, e `finance.payout.sent.v1` chega aqui (o `orders` marca as vendas como `invoiced`). O finance busca as vendas de cada pedido pago em `POST /internal/orders` daqui (`sales_for_finance`, só de `127.0.0.1`). Detalhes em `saci-finance/local-env/README.md` e `src/finance_bridge.rs`.

### Contas de exemplo

Senha de todas: `feira-de-exemplo`. Código de verificação (confirmação do contato, troca de contato, recuperação de senha): `123456`. A entrada aceita o e-mail ou o celular (`+5585988887777`, a conta de quem compra); o CPF não serve para entrar. Uma conta nova (cadastro com nome, e-mail ou celular e senha, sem CPF) abre na hora. O CPF completo de cada conta **aparece no terminal quando o servidor sobe**: no repositório ficam só os 9 primeiros dígitos (`sandbox/demo-accounts.json`), para nenhum CPF válido ficar escrito no código (a varredura de CPF da plataforma reprovaria). São os mesmos CPFs dos mocks da interface.

| Conta | Começo do CPF | Papel |
| --- | --- | --- |
| Marina (quem compra) | `529.982.247` | Compras, endereço, cartão salvo |
| Helena (responsável) | `390.533.447` | Dona do **Ateliê Barro Vivo** (`/artesaos/barro-vivo`): painel, vendas, chamados, equipe |
| Joana (equipe) | `714.602.380` | Equipe do Barro Vivo com `anuncios` e `pedidos` |

### Sandbox de pagamento

| Cartão | Resultado |
| --- | --- |
| Final `0002` (ex.: `4000 0000 0000 0002`) | Recusado (402 `card-declined`) |
| Qualquer outro válido (ex.: `4111 1111 1111 1111`) | Aprovado |
| Pix | Pago automaticamente 20 s depois (a tela de Pix vê `paid` ao consultar) |

### Outras simulações

| Rota (só no servidor de desenvolvimento) | Para |
| --- | --- |
| `POST /dev/assistant/handoffs` `{ "storefront_id", "first_name", "message" }` | A Caipora passar uma conversa para a vitrine (abre um chamado) |
| `POST /dev/carrier/sandbox` `{ "event_id", "tracking_code", "status": "delivered" }` | A transportadora avisar uma entrega (assinado pelo servidor) |
| `GET /dev/health` | Saúde |

Compra (#35): sem conta, o contato é confirmado antes de pagar (`POST /v1/identity/guest-contact-verifications` e `/{id}/confirm`, código `123456`); sem isso, `contact-verification-required`. Com `SACI_DEV_IDENTITY_URL`, o `saci-identity-dev` manda os eventos `identity.*` para o `POST /dev/events` daqui, e é assim que os pedidos de visitante chegam à conta.

KYC do sandbox (como o `kyc-gateway` do `saci-identity`, ADR 0010: a conferência responde na hora e a divergência só baixa a nota de fraude, lida em `fraud_score`): nascimento de menor de 18 anos → `underage` (a única recusa); conta sem CPF → `document-required`; nascimento em 1º de janeiro → nota com `birth_date` 0; CNPJ começando por `00` → `cnpj_active` 0, por `99` → `qsa_name` 0, por `88` → `address` 0, por `77` → `company_age` 0 (empresa aberta há menos de 30 dias).

### Teste ponta a ponta

```bash
task dev            # terminal 1
task dev:e2e        # terminal 2: 90 verificações pelos caminhos da interface
```

O roteiro (`scripts/dev-e2e.mjs`) cobre vitrine aberta, conta, compra com cartão, recusa, Pix, compra sem conta, painel, anúncio com foto pela esteira, vendas, etiqueta, entrega pela transportadora, equipe e chamados.

### Interface e contratos

A interface segue os contratos de `saci-contracts` e confere os tipos no `npm run typecheck` (`src/types/contractConformance.ts`). As divergências levantadas em `saci-contracts/docs/divergencias-da-interface.md` foram resolvidas na integração (#18): listas em `{ items }`, `next_cursor` ausente na última página, `If-Match` nas edições da vitrine e do anúncio, pacotes por vitrine nas compras e a confirmação da troca de contato.

Logotipo e capa enviados no servidor local saem em `http://localhost:8787/dev/cdn/...`. Fora da nuvem (ambiente `local` da interface), a origem do back-end local entra na lista de mídia confiável (S-22), então eles aparecem; na nuvem, só `https` em `cdn.saci.art.br`.

## LocalStack (a infraestrutura de verdade)

```bash
cd ../saci-boiuna && task local:up local:deploy && cd -   # plataforma: API Gateway, autorizador, barramento, buckets
task local:deploy                                          # compila os 10 domínios e aplica os stacks com envs/local.tfvars
task local:smoke                                           # rotas pelo API Gateway local, com o autorizador da plataforma
```

`local:deploy` aplica `infra/aws/stacks/shared` e, na ordem das dependências das migrações, `sellers → catalog → media → shipping → orders → checkout → customers → reviews → support → notifications`. As sementes rodam pela função `migrate` de cada domínio (`aws_lambda_invocation`).

Diferenças do LocalStack em relação à nuvem: sem VPC nem KMS (como o resto da plataforma no local), verificação de malware desligada (`malware_scan = "none"`: a notificação do S3 dispara a esteira), guest token assinado com a chave de desenvolvimento da plataforma (no SSM) em vez do KMS, e etiquetas com assinatura de desenvolvimento.

## Problemas comuns

| Sintoma | Causa provável |
| --- | --- |
| `failed to run custom build command for aws-lc-sys` (Windows) | Falta o NASM: use `AWS_LC_SYS_PREBUILT_NASM=1` (o `Taskfile.yml` já define) |
| `Uma política de Controle de Aplicativo bloqueou este arquivo` | O Controle Inteligente de Aplicativos do Windows bloqueou um executável recém-compilado; rode de novo ou libere a pasta `target/` |
| CORS recusado no site | A origem do Expo não está em `SACI_DEV_ORIGINS` (padrão: `localhost:8081` e `localhost:19006`) |
| `ParameterNotFound` no `local:deploy` | O ambiente local do `saci-boiuna` não foi aplicado |
