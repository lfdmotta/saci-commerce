# Operação

## Mensagens na fila de mensagens mortas (DLQ)

Cada assinatura de eventos (`saci-<env>-<domínio>-<assinatura>`) tem DLQ com alarme (`alarm_topic_arn`).

1. Veja o motivo nos logs da função consumidora (`/aws/lambda/saci-<env>-<domínio>-<função>`): "evento malformado" não se conserta repetindo; "falha passageira" sim.
2. Corrigida a causa, devolva as mensagens: console do SQS → *Start DLQ redrive*, ou `aws sqs start-message-move-task --source-arn <dlq-arn>`.
3. Os consumidores deduplicam pelo `id` do evento: repetir é seguro.

## Saga de compra parada

O `sweeper` compensa a cada 5 minutos as sagas sem resposta há mais de 2 minutos e os Pix vencidos sem aviso. Para conferir um pedido:

```bash
aws dynamodb get-item --table-name saci-<env>-checkout --key '{"pk":{"S":"SAGA#<ord_...>"},"sk":{"S":"META"}}'
aws lambda invoke --function-name saci-<env>-checkout-sweeper --qualifier live /dev/stdout
```

Estados: `started` → `awaiting_payment` → `confirmed` ou `abandoned`. Reserva de estoque presa sem saga: `RSV#<pedido>` na tabela do catálogo; a operação interna `release` solta (idempotente).

## Busca desatualizada

A projeção é refeita do zero sem perda (dado público):

```bash
aws lambda invoke --function-name saci-<env>-catalog-search-internal --qualifier live \
  --payload '{"op":"rebuild","payload":{}}' --cli-binary-format raw-in-base64-out /dev/stdout
```

A função `search` relê a projeção a cada 30 s (`SEARCH_SNAPSHOT_SECS`).

## Imagem presa em `scanning`

Na nuvem, a esteira só roda quando o GuardDuty publica o resultado. Confira o plano de proteção do balde `saci-<env>-media-quarantine-<conta>` e a regra `saci-<env>-media-scan-result`. Objeto sem resultado em 1 dia é apagado pelo ciclo de vida, e a pessoa envia de novo.

## Remover alguém da equipe com urgência

`DELETE /v1/sellers/me/team/{id}` tira o papel na tabela `authz` (efeito em até 60 s, o cache do autorizador). Para efeito imediato, revogue as sessões da conta como no runbook `revoke-sessions.md` do `saci-boiuna`.

## Migrações

Toda implantação invoca `saci-<env>-<domínio>-migrate`; cada migração aplicada fica em `MIGRATION`/`<id>` na tabela do domínio e não roda de novo. Migração nova é sempre para frente (seção 9.7) e entra no `src/seed.rs` do serviço.
