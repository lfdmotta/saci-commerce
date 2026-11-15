/**
 * # reviews
 *
 * Domínio `reviews` (proposta 5.12). A proposta (3.1) permite nascer junto do
 * catálogo; aqui já nasce separado (tabela, chave e papéis próprios), porque o
 * custo é o mesmo e a fronteira de dados fica explícita desde o início.
 */

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "reviews"
  data_class  = "C2"

  tables = {
    main = { data_class = "C2" }
  }

  functions = {
    api     = { role = "api", description = "reviews-api: avaliações e resumo", tables_write = ["main"], invoke_internal = true }
    migrate = { role = "migrate", description = "Avaliações de exemplo (fora de produção)", timeout_seconds = 300, tables_write = ["main"] }
  }

  routes = {
    "GET /v1/reviews/storefronts/{url_name}" = { access = "public", function = "api" }
    "POST /v1/reviews/me/reviews"            = { access = "user", function = "api" }
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
