# ADR 0004: Chamadas internas por invocação direta de Lambda

- **Situação:** aceita
- **Data:** 2026-11-15

## Contexto

A proposta (3.2, 7.4) prevê chamadas síncronas entre serviços por VPC Lattice com IAM, e a plataforma deixou a confirmação das políticas do Lattice em `sa-east-1` para uma prova de conceito. A saga do checkout precisa de respostas na hora (estoque, pedido, cobrança).

## Decisão

- Cada domínio que responde a outros expõe uma função `*-internal` com operações nomeadas (`{"op", "payload"}` → `{"ok"}` ou `{"error"}`). Quem chama invoca a função pelo IAM (`lambda:InvokeFunction` só nos ARNs declarados), sem rede pública e sem rota no API Gateway.
- No código, é a porta `InternalClient`: invocação na nuvem, chamada em processo no servidor de desenvolvimento, *sandbox* para os domínios de outros repositórios fora de produção.
- As operações estão em [contratos-internos.md](../contratos-internos.md).

## Consequências

- Autenticação e autorização entre serviços por IAM, sem segredo compartilhado.
- Dentro da conta Prod. Para a conta de pagamentos (Payments-Prod), a invocação entre contas exige política de recurso na função do `saci-payments` — ou o Lattice, se a prova de conceito confirmar; a porta não muda.
