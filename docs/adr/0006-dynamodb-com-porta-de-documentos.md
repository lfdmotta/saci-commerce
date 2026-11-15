# ADR 0006: DynamoDB com uma porta de documentos em todos os serviços

- **Situação:** aceita
- **Data:** 2026-11-15

## Contexto

A proposta usa DynamoDB na maioria dos serviços e Aurora PostgreSQL em `orders` (transações e relatórios). Aurora é custo fixo, sub-rede isolada e migrações de DDL; os acessos do `orders` são por chave (pedido, venda, conta, vitrine), e a transação que importa (estoque da peça única) já é condicional no catálogo.

## Decisão

- Todos os serviços usam a porta `DocumentStore`: documentos JSON por `pk`/`sk`, escrita condicional por versão (também o `ETag` das edições), transações de várias escritas e o *outbox* na mesma transação. Índices secundários são documentos gravados na mesma transação, sem GSI.
- O adaptador DynamoDB usa o módulo `dynamodb-table` da plataforma e grava o *outbox* no formato que o `saci-outbox` retransmite.

## Consequências

- Um só modelo de dados e de testes; o servidor de desenvolvimento usa o adaptador em memória.
- Relatórios do `orders` saem de exportações para o S3 quando forem necessários; trocar o `orders` para Aurora é escrever outro adaptador da porta (ou um repositório próprio do serviço), sem mudar rotas.
- `migrations/` guarda as **sementes e correções de dados**, aplicadas pela função `migrate` depois de cada implantação, idempotentes e registradas em `MIGRATION`.
