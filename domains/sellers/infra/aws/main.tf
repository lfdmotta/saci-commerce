/**
 * # sellers
 *
 * Domínio `sellers` (proposta 5.3). Além da própria tabela, as funções que
 * mexem na equipe gravam o papel de vitrine na tabela `authz` da **plataforma**
 * (SSM `/saci/<env>/gateway/authz-table`), que o autorizador lê: só
 * `PutItem`/`DeleteItem` nos itens `SUBJECT#*`/`GRANT`.
 */

data "aws_caller_identity" "current" {}

data "aws_ssm_parameter" "authz_table" {
  name = "/saci/${var.environment}/gateway/authz-table"
}

locals {
  authz_table_arn = "arn:aws:dynamodb:${var.region}:${data.aws_caller_identity.current.account_id}:table/${data.aws_ssm_parameter.authz_table.value}"

  # Só os itens de papel (`GRANT`) da tabela da plataforma.
  grants_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "PlatformGrants"
      Effect    = "Allow"
      Action    = ["dynamodb:PutItem", "dynamodb:DeleteItem", "dynamodb:GetItem"]
      Resource  = local.authz_table_arn
      Condition = { "ForAllValues:StringLike" = { "dynamodb:LeadingKeys" = ["SUBJECT#*"] } }
    }]
  })

  env = {
    AUTHZ_TABLE  = data.aws_ssm_parameter.authz_table.value
    CDN_BASE_URL = var.cdn_base_url
  }
}

variable "cdn_base_url" {
  description = "Base pública do CDN (S-22: logotipo e capa só deste host)."
  type        = string
  default     = "https://cdn.saci.art.br"
}

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "sellers"
  data_class  = "C2"

  tables = {
    main = { data_class = "C2", stream = true }
  }

  functions = {
    api       = { role = "api", description = "sellers-api: vitrines, abertura, painel e equipe", tables_write = ["main"], invoke_internal = true, environment = local.env, extra_policy_json = local.grants_policy }
    internal  = { role = "internal", description = "Perfil, endereço e equipe das vitrines para outros domínios", tables_read = ["main"], invoke_internal = true, environment = local.env }
    workspace = { role = "workspace", description = "Contagens do painel e convites pendentes", timeout_seconds = 30, tables_write = ["main"], invoke_internal = true, environment = local.env }
    relay     = { role = "relay", description = "Outbox das vitrines para o barramento", tables_read = ["main"] }
    migrate   = { role = "migrate", description = "Pares de letras e vitrines de lançamento", timeout_seconds = 300, tables_write = ["main"], invoke_internal = true, environment = local.env, extra_policy_json = local.grants_policy }
  }

  routes = {
    "GET /v1/sellers/storefronts/{url_name}"                       = { access = "public", function = "api" }
    "GET /v1/sellers/storefront-fonts"                             = { access = "public", function = "api" }
    "GET /v1/sellers/storefronts/availability"                     = { access = "user", function = "api" }
    "POST /v1/sellers/onboarding/identity"                         = { access = "user", function = "api" }
    "POST /v1/sellers/onboarding/business"                         = { access = "user", function = "api" }
    "POST /v1/sellers/storefronts"                                 = { access = "user", function = "api" }
    "GET /v1/sellers/me/workspace"                                 = { access = "user", function = "api" }
    "GET /v1/sellers/me/storefront"                                = { access = "user", function = "api" }
    "PUT /v1/sellers/me/storefront"                                = { access = "user", function = "api" }
    "GET /v1/sellers/me/team"                                      = { access = "user", function = "api" }
    "POST /v1/sellers/me/team"                                     = { access = "user", function = "api" }
    "PATCH /v1/sellers/me/team/{member_id}"                        = { access = "user", function = "api" }
    "DELETE /v1/sellers/me/team/{member_id}"                       = { access = "user", function = "api" }
    "GET /v1/sellers/me/team-invitations"                          = { access = "user", function = "api" }
    "POST /v1/sellers/me/team-invitations/{invitation_id}/accept"  = { access = "user", function = "api" }
    "POST /v1/sellers/me/team-invitations/{invitation_id}/decline" = { access = "user", function = "api" }
  }

  subscriptions = {
    workspace = {
      function    = "workspace"
      description = "Contagens do painel e convites"
      event_types = [
        "catalog.listing.published.v1", "catalog.listing.paused.v1", "catalog.listing.withdrawn.v1", "catalog.stock.changed.v1",
        "checkout.order.confirmed.v1", "orders.sale.ready.v1", "orders.sale.shipped.v1", "orders.sale.delivered.v1", "orders.return.opened.v1",
        "support.ticket.opened.v1", "support.ticket.resolved.v1", "identity.account.verified.v1",
      ]
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
