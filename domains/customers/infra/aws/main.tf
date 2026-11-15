/**
 * # customers
 *
 * Domínio `customers` (proposta 5.2). Guarda nome preferido, referência do
 * endereço no cofre e favoritos (C2); CPF, contato e endereço ficam no cofre
 * do `saci-identity` (D9).
 */

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "customers"
  data_class  = "C2"

  tables = {
    main = { data_class = "C2" }
  }

  functions = {
    api      = { role = "api", description = "customers-api: perfil, endereço, favoritos, novidades", tables_write = ["main"], invoke_internal = true }
    profiles = { role = "profiles", description = "Perfil criado na conta verificada", tables_write = ["main"], invoke_internal = true }
  }

  routes = {
    "GET /v1/customers/me"                           = { access = "user", function = "api" }
    "PATCH /v1/customers/me"                         = { access = "user", function = "api" }
    "PUT /v1/customers/me/address"                   = { access = "user", function = "api" }
    "GET /v1/customers/me/favorites"                 = { access = "user", function = "api" }
    "PUT /v1/customers/me/favorites/{listing_id}"    = { access = "user", function = "api" }
    "DELETE /v1/customers/me/favorites/{listing_id}" = { access = "user", function = "api" }
    "POST /v1/customers/newsletter-subscriptions"    = { access = "public", function = "api" }
  }

  subscriptions = {
    profiles = { function = "profiles", description = "Conta verificada", event_types = ["identity.account.verified.v1"] }
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

output "functions" {
  description = "Funções implantadas."
  value       = module.domain.function_names
}

output "deployment_trigger" {
  description = "Mudou: reimplante o estágio do API Gateway."
  value       = module.domain.deployment_trigger
}
