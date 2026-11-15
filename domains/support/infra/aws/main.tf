/**
 * # support
 *
 * Domínio `support` (proposta 5.11). Textos de quem compra guardados
 * mascarados (S-23); retenção a definir com o jurídico (6.3: 5 anos, a validar).
 */

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "support"
  data_class  = "C2"

  tables = {
    main = { data_class = "C2", stream = true }
  }

  functions = {
    api      = { role = "api", description = "support-api: chamados", tables_write = ["main"], invoke_internal = true }
    internal = { role = "internal", description = "Chamados abertos para o painel", tables_read = ["main"] }
    handoffs = { role = "handoffs", description = "Abre o chamado no repasse da Caipora", timeout_seconds = 30, tables_write = ["main"], invoke_internal = true }
    relay    = { role = "relay", description = "Outbox dos chamados para o barramento", tables_read = ["main"] }
    migrate  = { role = "migrate", description = "Chamados de exemplo (fora de produção)", timeout_seconds = 120, tables_write = ["main"] }
  }

  routes = {
    "GET /v1/support/me/tickets"                      = { access = "user", function = "api" }
    "GET /v1/support/me/tickets/{protocol}"           = { access = "user", function = "api" }
    "POST /v1/support/me/tickets/{protocol}/messages" = { access = "user", function = "api" }
    "POST /v1/support/me/tickets/{protocol}/resolve"  = { access = "user", function = "api" }
  }

  subscriptions = {
    handoffs = { function = "handoffs", description = "Repasse da Caipora", event_types = ["assistant.handoff.requested.v1"] }
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
