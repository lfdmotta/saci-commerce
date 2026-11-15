# ADR 0007: Saga do checkout em Rust, com compensação e varredura

- **Situação:** aceita
- **Data:** 2026-11-15

## Contexto

A proposta (3.3) desenha o checkout como saga em Step Functions. A resposta da compra é síncrona (201 com o pedido, 402 recusado, 409 sem estoque ou preço mudou), e os passos são chamadas a outros domínios.

## Decisão

- A saga é uma função Rust (`checkout-saga`) que orquestra as chamadas internas com **compensação em cada passo**: falha depois da reserva solta o estoque; falha depois do pedido também cancela.
- O estado da saga fica na tabela do checkout (só ids e referências: CPF e endereço vão antes para o cofre, D9). Sagas paradas são compensadas por uma **varredura agendada** a cada 5 minutos, e a reserva de estoque tem prazo como última garantia.
- O Pix é concluído por evento (`payments.pix.paid.v1`) ou na consulta do `GET` (o webhook pode se perder).

## Consequências

- Menos latência e custo que uma execução síncrona do Step Functions Express, e a mesma lógica roda no servidor de desenvolvimento e nos testes.
- A durabilidade vem da tabela, da varredura e da idempotência de cada passo (`reserve`, `create_order`, `charge` são idempotentes por pedido), não do motor de fluxo.
- Se a saga crescer (personalização, devolução), Step Functions entra como outro orquestrador dos mesmos passos.
