# Contratos internos

As chamadas **síncronas** entre domínios (seção 3.2: "só onde a resposta é necessária na hora"). Não passam pelo API Gateway: são invocações diretas da função `saci-<env>-<domínio>-internal`, autorizadas por IAM ([ADR 0004](adr/0004-chamadas-internas-por-invocacao.md)).

No fio: `{"op": "<operação>", "payload": {...}}` → `{"ok": {...}}` ou `{"error": <Problem RFC 9457>}`. A recusa volta para quem chamou a API com o mesmo `code`.

## Oferecidas por este repositório

| Alvo | Operação | Entrada | Saída | Quem chama |
| --- | --- | --- | --- | --- |
| `catalog` | `checkout_items` | `{ items: [{ listing_id, quantity }] }` | `{ items: [{ listing_id, available, title, storefront_id, storefront_name, region, unit_price, stock, production_days, cover_url, ships_abroad }] }` | checkout, shipping |
| `catalog` | `reserve` | `{ order_id, items, ttl_secs }` | `{}` ou 409 `item-unavailable` | checkout |
| `catalog` | `commit`, `release` | `{ order_id }` | `{}` (idempotentes) | checkout |
| `catalog` | `public_listings` | `{ ids }` (até 48) | `{ items: [Listing] }` | customers |
| `catalog` | `active_listings` | `{ storefront_id }` | `{ count }` | sellers |
| `search` | `reindex_storefront` | `{ storefront_id }` | `{}` | catalog |
| `search` | `rebuild` | `{}` | `{ storefronts }` | catalog (`migrate`) |
| `sellers` | `storefront_profile` | `{ storefront_id }` | `{ storefront_id, brand_name, url_name, slogan, city, state, theme }` | catalog |
| `sellers` | `storefront_by_url` | `{ url_name }` | `{ storefront_id, brand_name, url_name }` | reviews |
| `sellers` | `storefront_summary` | `{ storefront_id }` | `{ storefront_id, brand_name, url_name, owner_account_id, logo_url }` | orders, notifications |
| `sellers` | `storefront_members` | `{ storefront_id }` | `{ accounts: [{ account_id, role, permissions }] }` | notifications |
| `shipping` | `postal_code` | `{ postal_code }` | `PostalCodeInfo` ou 404 | sellers |
| `shipping` | `quote` | `ShippingQuoteRequest` | `{ items: [ShippingQuote] }` | checkout |
| `media` | `uploads_for_listing` | `{ storefront_id, upload_ids }` | `{ items: [{ upload_id, status, public_url, width, height }] }` (`missing` se não é da vitrine) | catalog, sellers |
| `orders` | `create_order` | `{ order_id, account_id?, payment_method, address_ref, destination, buyer: { first_name, city, state }, lines, shipping }` | `{ order }` | checkout |
| `orders` | `mark_paid`, `cancel` | `{ order_id }` | `{ order }` | checkout |
| `orders` | `order_status` | `{ order_id }` | `{ order, status, account_id }` | checkout |
| `orders` | `listing_has_open_orders` | `{ listing_id }` | `{ open }` | catalog |
| `orders` | `storefront_to_do` | `{ storefront_id }` | `{ count }` | sellers |
| `orders` | `sale_for_review` | `{ sale_id, account_id }` | `{ storefront_id, delivered, city, state }` ou 404 | reviews |
| `orders` | `sale_summary` | `{ sale_id }` | `{ storefront_id, status, number, tracking_code }` | shipping |
| `orders` | `sale_for_label` | `{ sale_id }` | `{ storefront_id, number, service, sender, recipient, postal_code }` (só em `awaiting_shipment`) | shipping |
| `orders` | `sale_parties`, `order_parties` | `{ sale_id }` / `{ order_id }` | conta e vitrine | notifications |
| `orders` | `sales_for_finance` | `{ order_id }` ou `{ sale_id }` | `{ items: [{ sale_id, order_id, number, storefront_id, placed_at, status, items_total, quantity, shipping: { service, price, delivered_at? }, destination_state? }] }` | finance |
| `support` | `open_tickets` | `{ storefront_id }` | `{ count }` | sellers |

## Esperadas de outros repositórios

Hoje atendidas pelo **sandbox** (`crates/sandbox`, só fora de produção). Quando cada repositório existir, a função dele responde no mesmo formato e o stack aponta `external_functions` para ela.

### `saci-identity` (alvo `identity`: cofre de dados pessoais, D9)

| Operação | Entrada | Saída | Regra |
| --- | --- | --- | --- |
| `account_profile` | `{ account_id }` | `{ id, name?, preferred_name?, document_type, document_masked, cpf_masked, identity_status, email?, phone?, pending_contact?, created_at }` | Só o mascarado sai do cofre |
| `lookup_cpf` | `{ cpf }` | `{ fingerprint, account_id? }` | Digital por HMAC com chave do cofre |
| `cpf_fingerprint_of` | `{ account_id }` | `{ fingerprint? }` (`null` na conta sem CPF) | Para casar convites pendentes; sem CPF, o `sellers` espera o `identity.account.document_added.v1` |
| `put_address` | `{ owner_account_id, purpose: delivery\|business, address }` | `{ ref }` (`addr_…`) | Endereço cifrado no cofre |
| `open_address` | `{ ref, purpose: sale\|label\|business\|delivery, … }` | `{ address }` | Regra por quem pede (tabela da seção 9.3) |
| `default_address` | `{ account_id }` | `{ ref? }` | Endereço já conhecido da conta |
| `put_guest_buyer` | `{ order_id, cpf?, name, email?, phone? }` | `{}` | Compra sem conta: nada disso fica no checkout. O CPF é opcional ("CPF na nota fiscal?", #27); o celular vai em E.164 |
| `request_contact_change` | `{ account_id, email?, phone? }` | `{}` ou `contact-required` | Confirmação no destino novo, aviso no antigo (S-12) |
| `notify`, `send_to_address` | `{ account_id \| email, template, token? }` | `{}` | E-mail e SMS saem pelo cofre |

### `saci-identity` (alvo `kyc`: `kyc-gateway`)

| Operação | Entrada | Saída |
| --- | --- | --- |
| `check_identity` | `{ account_id, birth_date }` | `{ result: ok\|underage, status: pending }`; `422 document-required` (conta sem CPF) |
| `check_business` | `{ account_id, cnpj, address?: { postal_code, street?, city, state } }` | `{ result: ok, status: pending, cnpj_masked }` |
| `fraud_score` | `{ account_id }` | `{ status: none\|pending\|done, score?, identity?, business? }` |

A consulta à Receita é assíncrona (`saci-identity`, ADR 0010): as conferências respondem na hora, e a divergência baixa a nota de fraude da conta (0 a 100) em vez de recusar. O `sellers` só recusa menor de idade. O endereço do ateliê vem do CEP informado em `POST /v1/sellers/onboarding/business` (`postal_code`), completado pelo `shipping`.

### `saci-payments` (alvo `payments`)

O repositório já existe: o contrato formal está em `saci-contracts/internal/payments.yaml` (com `refund`, `payout`, `payout_status` e `payment_records` além das duas abaixo), e no servidor de desenvolvimento a ponte `SACI_DEV_PAYMENTS_URL` troca o sandbox pelo `saci-payments-dev`. Na nuvem, falta apontar `external_functions.payments` para a função `saci-<env>-payments-internal` (saída `internal_function_arn` do stack do domínio de lá). `charge` também pode responder `paid`, que o checkout já trata.


| Operação | Entrada | Saída |
| --- | --- | --- |
| `charge` | `{ order_id, account_id?, method, amount, card_token?, saved_card_id?, save_card, splits: [{ storefront_id, amount }] }` | `{ charge_id, status: authorized\|declined\|pending, pix?: { code, expires_at }, next_action? }` (idempotente por `order_id`) |
| `charge_status` | `{ order_id }` | `{ status: authorized\|pending\|paid\|expired\|failed, charge_id }` |

E os eventos `payments.{charge.authorized,charge.failed,pix.paid,pix.expired}.v1`, que o `checkout` consome.

### `saci-finance` (alvo `finance`)

O repositório já existe: o contrato formal está em `saci-contracts/internal/finance.yaml` (com as operações do painel interno além da abaixo), e no servidor de desenvolvimento a ponte `SACI_DEV_FINANCE_URL` troca o sandbox pelo `saci-finance-dev`. Na nuvem, falta apontar `external_functions.finance` para a função `saci-<env>-finance-internal` (saída `internal_function_arn` do stack de lá) e pôr o papel do `orders` em `internal_principals` de lá. O finance chama `sales_for_finance` daqui.

| Operação | Entrada | Saída |
| --- | --- | --- |
| `sale_payment` | `{ sale_id, storefront_id, items_total, status }` | `{ fee, net, payout_date? }` (S-10: só para a pessoa responsável) |

E o evento `finance.payout.sent.v1` (`orders` marca as vendas como `invoiced`).

### `saci-assistant` (alvo `assistant`)

| Operação | Entrada | Saída |
| --- | --- | --- |
| `handoff_details` | `{ conversation_id }` | `{ subject, summary, customer: { first_name, city?, state? }, order_number?, messages: [{ id, author, author_name?, text, created_at }] }` |

E o evento `assistant.handoff.requested.v1` (`support` abre o chamado).

## Rotas HTTP que o sandbox atende para o teste local

O servidor de desenvolvimento também responde, com o sandbox, as rotas de `/v1/identity` (cadastro com código, entrada, refresh rotativo com cookie `HttpOnly` no site, reautenticação, saída, recuperação de senha) e de `/v1/payments` (`client-config`, preferências, cartões, tokenização em `/dev/psp/tokens`), nos formatos de `saci-contracts/http/identity.yaml` e da seção 5.9, para a interface poder rodar inteira contra o back-end local.
