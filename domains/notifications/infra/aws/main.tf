/**
 * # notifications
 *
 * Domínio `notifications` (proposta 9.6): aparelhos e preferências, o
 * despachante (eventos → avisos) e o enviador (fila → Expo Push), separados
 * para o envio escalar em paralelo sem segurar o consumo de eventos.
 */

# A fila leva o token do aparelho (C2): cifrada com a chave do domínio (SSE-SQS só no local).
resource "aws_sqs_queue" "push_dlq" {
  name                      = "saci-${var.environment}-notifications-push-dlq"
  message_retention_seconds = 4 * 24 * 3600
  kms_master_key_id         = module.domain.key_arn
  sqs_managed_sse_enabled   = module.domain.key_arn == null ? true : null
}

resource "aws_sqs_queue" "push" {
  name                       = "saci-${var.environment}-notifications-push"
  visibility_timeout_seconds = 60
  message_retention_seconds  = 24 * 3600
  kms_master_key_id          = module.domain.key_arn
  sqs_managed_sse_enabled    = module.domain.key_arn == null ? true : null
  redrive_policy             = jsonencode({ deadLetterTargetArn = aws_sqs_queue.push_dlq.arn, maxReceiveCount = 5 })
}

locals {
  env = { PUSH_QUEUE_URL = aws_sqs_queue.push.id, PUSH_PROVIDER = "log" }
}

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "notifications"
  data_class  = "C2"

  tables = {
    main = { data_class = "C2" }
  }

  functions = {
    api         = { role = "api", description = "notifications-api: aparelhos e preferências", tables_write = ["main"] }
    dispatcher  = { role = "dispatcher", description = "Eventos → avisos por aparelho", timeout_seconds = 30, tables_read = ["main"], invoke_internal = true, environment = local.env, extra_policy_json = jsonencode({ Version = "2012-10-17", Statement = [{ Sid = "Enqueue", Effect = "Allow", Action = ["sqs:SendMessage"], Resource = aws_sqs_queue.push.arn }] }) }
    push-sender = { role = "push-sender", description = "Envio dos avisos ao Expo Push", timeout_seconds = 20, tables_write = ["main"], environment = local.env, extra_policy_json = jsonencode({ Version = "2012-10-17", Statement = [{ Sid = "Consume", Effect = "Allow", Action = ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes", "sqs:ChangeMessageVisibility"], Resource = aws_sqs_queue.push.arn }] }) }
  }

  routes = {
    "PUT /v1/notifications/me/devices/{installation_id}"    = { access = "user", function = "api" }
    "DELETE /v1/notifications/me/devices/{installation_id}" = { access = "user", function = "api" }
    "GET /v1/notifications/me/preferences"                  = { access = "user", function = "api" }
    "PUT /v1/notifications/me/preferences"                  = { access = "user", function = "api" }
  }

  subscriptions = {
    dispatcher = {
      function    = "dispatcher"
      description = "Avisos de pedidos, vendas, chamados e repasses"
      event_types = ["checkout.order.confirmed.v1", "orders.sale.shipped.v1", "orders.sale.delivered.v1", "support.ticket.opened.v1", "finance.payout.sent.v1"]
    }
  }

  internal_targets = local.internal_targets
  sandbox_table    = local.sandbox_table

  lambda_zip               = var.lambda_zip
  lambda_architecture      = var.lambda_architecture
  lambda_extra_environment = var.lambda_extra_environment
  permissions_boundary_arn = var.permissions_boundary_arn
  log_retention_days       = var.log_retention_days
  alarm_topic_arn          = var.alarm_topic_arn
}

resource "aws_lambda_event_source_mapping" "push" {
  event_source_arn        = aws_sqs_queue.push.arn
  function_name           = "${module.domain.function_names["push-sender"]}:live"
  batch_size              = 10
  function_response_types = ["ReportBatchItemFailures"]
}

output "functions" {
  description = "Funções implantadas."
  value       = module.domain.function_names
}

output "deployment_trigger" {
  description = "Mudou: reimplante o estágio do API Gateway."
  value       = module.domain.deployment_trigger
}
