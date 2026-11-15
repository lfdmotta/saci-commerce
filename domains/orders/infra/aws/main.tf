/**
 * # orders
 *
 * Domínio `orders` (proposta 5.8). A proposta prevê Aurora PostgreSQL para
 * relatórios; o começo usa DynamoDB com a mesma porta dos outros serviços
 * (ADR 0006), e os relatórios saem de exportações para o S3 quando o volume
 * pedir.
 */

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "orders"
  data_class  = "C2"

  tables = {
    main = { data_class = "C2", stream = true }
  }

  functions = {
    api      = { role = "api", description = "orders-api: compras e vendas", tables_write = ["main"], invoke_internal = true }
    internal = { role = "internal", description = "Pedido do checkout e leituras de vendas para outros domínios", tables_write = ["main"], invoke_internal = true }
    progress = { role = "progress", description = "Etiqueta, entrega e repasse por eventos", timeout_seconds = 30, tables_write = ["main"], invoke_internal = true }
    relay    = { role = "relay", description = "Outbox dos pedidos para o barramento", tables_read = ["main"] }
    migrate  = { role = "migrate", description = "Compras e vendas de exemplo (fora de produção)", timeout_seconds = 300, tables_write = ["main"], invoke_internal = true }
  }

  routes = {
    "GET /v1/orders/me/purchases"                 = { access = "user", function = "api" }
    "GET /v1/orders/me/purchases/{order_id}"      = { access = "user", function = "api" }
    "GET /v1/orders/guest/{order_id}"             = { access = "mixed", function = "api" }
    "GET /v1/orders/me/sales"                     = { access = "user", function = "api" }
    "GET /v1/orders/me/sales/{sale_id}"           = { access = "user", function = "api" }
    "POST /v1/orders/me/sales/{sale_id}/ready"    = { access = "user", function = "api" }
    "POST /v1/orders/me/sales/{sale_id}/shipment" = { access = "user", function = "api" }
  }

  subscriptions = {
    progress = {
      function    = "progress"
      description = "Ciclo das vendas"
      event_types = ["shipping.label.purchased.v1", "shipping.tracking.updated.v1", "finance.payout.sent.v1"]
    }
  }

  internal_targets = local.internal_targets
  sandbox_table    = local.sandbox_table
  seed_fixtures    = var.seed_fixtures

  lambda_zip               = var.lambda_zip
  lambda_architecture      = var.lambda_architecture
  lambda_extra_environment = var.lambda_extra_environment
  permissions_boundary_arn = var.permissions_boundary_arn
  log_retention_days       = var.log_retention_days
  alarm_topic_arn          = var.alarm_topic_arn
}

output "functions" {
  description = "Funções implantadas."
  value       = module.domain.function_names
}

output "deployment_trigger" {
  description = "Mudou: reimplante o estágio do API Gateway."
  value       = module.domain.deployment_trigger
}
