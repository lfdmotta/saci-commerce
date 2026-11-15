/**
 * # checkout
 *
 * Domínio `checkout` (proposta 3.3 e 5.7; ADR 0007: saga em Rust).
 *
 * - `POST /v1/checkout` é **público na borda** (compra sem conta) e a função
 *   confere o `Authorization`, quando vier, com o código do autorizador da
 *   plataforma: lê emissores e revogação da tabela `authz` (só leitura).
 * - O **guest token** é assinado por uma chave assimétrica do KMS (ECC P-256),
 *   que nunca sai do KMS. A chave pública (saída `guest_public_key_pem`) entra
 *   como emissor `guest` em `trusted_issuers` do stack `gateway/aws/stacks/api`
 *   da plataforma. No local, a chave vem do SSM (`/saci/local/commerce/guest-key`),
 *   gravada pelo `deploy.sh` a partir das chaves de desenvolvimento da plataforma.
 * - A varredura agendada compensa sagas paradas a cada 5 minutos.
 */

data "aws_caller_identity" "current" {}

data "aws_ssm_parameter" "authz_table" {
  name = "/saci/${var.environment}/gateway/authz-table"
}

variable "trusted_issuers" {
  description = "Emissores aceitos na conta opcional (os mesmos do autorizador da plataforma)."
  type        = list(string)
  default     = []
}

variable "guest_issuer" {
  description = "`iss` do guest token, igual ao cadastrado na tabela `authz`."
  type        = string
  default     = "https://checkout.saci.art.br"
}

variable "guest_audience" {
  description = "Audiência do guest token."
  type        = string
  default     = "saci-api"
}

variable "guest_ttl_secs" {
  description = "Validade do guest token (a proposta pede 30 dias; limitado pela vida máxima do emissor na plataforma)."
  type        = number
  default     = 2592000
}

locals {
  is_local  = var.environment == "local"
  authz_arn = "arn:aws:dynamodb:${var.region}:${data.aws_caller_identity.current.account_id}:table/${data.aws_ssm_parameter.authz_table.value}"

  env = merge(
    {
      AUTHZ_TABLE     = data.aws_ssm_parameter.authz_table.value
      TRUSTED_ISSUERS = join(",", var.trusted_issuers)
      GUEST_ISSUER    = var.guest_issuer
      GUEST_AUDIENCE  = var.guest_audience
      GUEST_TTL_SECS  = tostring(var.guest_ttl_secs)
      GUEST_KID       = local.is_local ? "local-guest" : "guest-${var.environment}-1"
    },
    local.is_local ? { GUEST_KEY_PARAM = "/saci/local/commerce/guest-key" } : { GUEST_KMS_KEY_ID = aws_kms_key.guest[0].arn },
  )

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = concat(
      [{ Sid = "AuthzRead", Effect = "Allow", Action = ["dynamodb:GetItem", "dynamodb:BatchGetItem"], Resource = local.authz_arn }],
      local.is_local ? [] : [{ Sid = "GuestToken", Effect = "Allow", Action = ["kms:Sign"], Resource = aws_kms_key.guest[0].arn }],
    )
  })
}

resource "aws_kms_key" "guest" {
  count                    = local.is_local ? 0 : 1
  description              = "Assina o guest token do checkout (ES256)"
  key_usage                = "SIGN_VERIFY"
  customer_master_key_spec = "ECC_NIST_P256"
  deletion_window_in_days  = 30
  tags                     = { "saci:data-class" = "C1" }
}

resource "aws_kms_alias" "guest" {
  count         = local.is_local ? 0 : 1
  name          = "alias/saci-${var.environment}-checkout-guest"
  target_key_id = aws_kms_key.guest[0].key_id
}

data "aws_kms_public_key" "guest" {
  count  = local.is_local ? 0 : 1
  key_id = aws_kms_key.guest[0].arn
}

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "checkout"
  data_class  = "C2"

  tables = {
    main = { data_class = "C2", stream = true }
  }

  functions = {
    api      = { role = "api", description = "checkout-saga: a compra", timeout_seconds = 25, memory_mb = 512, tables_write = ["main"], invoke_internal = true, environment = local.env, extra_policy_json = local.policy, reserved_concurrency = 50 }
    payments = { role = "payments", description = "Pix pago ou vencido, cobrança falha", timeout_seconds = 30, tables_write = ["main"], invoke_internal = true, environment = local.env, extra_policy_json = local.policy }
    sweeper  = { role = "sweeper", description = "Compensa sagas paradas", timeout_seconds = 120, tables_write = ["main"], invoke_internal = true, environment = local.env, extra_policy_json = local.policy }
    relay    = { role = "relay", description = "Outbox da compra para o barramento", tables_read = ["main"] }
  }

  routes = {
    "POST /v1/checkout"                          = { access = "public", function = "api" }
    "GET /v1/checkout/orders/{order_id}/payment" = { access = "mixed", function = "api" }
  }

  subscriptions = {
    payments = {
      function    = "payments"
      description = "Resultado das cobranças"
      event_types = ["payments.pix.paid.v1", "payments.charge.authorized.v1", "payments.pix.expired.v1", "payments.charge.failed.v1"]
    }
  }

  schedules = {
    sweeper = { function = "sweeper", expression = "rate(5 minutes)" }
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

output "guest_public_key_pem" {
  description = "Chave pública do guest token, para o emissor `guest` da plataforma (nula no local)."
  value       = try(data.aws_kms_public_key.guest[0].public_key_pem, null)
}

output "deployment_trigger" {
  description = "Mudou: reimplante o estágio do API Gateway."
  value       = module.domain.deployment_trigger
}
