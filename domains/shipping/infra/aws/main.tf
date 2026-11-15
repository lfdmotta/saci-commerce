/**
 * # shipping
 *
 * Domínio `shipping` (proposta 5.6):
 *
 * - etiquetas em `labels/` no balde privado de `files.` da plataforma (SSM
 *   `/saci/<env>/edge/bucket/files`, SSE-KMS C3), entregues por URL assinada
 *   do CloudFront de 5 minutos (S-09). A chave privada do grupo de chaves fica
 *   no SSM (`files_private_key_param`); sem ela (local, dev), assinatura de
 *   desenvolvimento;
 * - a entrada `hooks.` da transportadora (`carrier-webhooks`): API Gateway
 *   **regional com mTLS** e domínio próprio, criado quando houver contrato com a
 *   transportadora (`enable_carrier_hooks`), a única origem pública além do
 *   CloudFront (2.3).
 */

data "aws_caller_identity" "current" {}

data "aws_ssm_parameter" "files_bucket" {
  name = "/saci/${var.environment}/edge/bucket/files"
}

variable "files_base_url" {
  description = "Base pública de `files.`."
  type        = string
  default     = "https://files.saci.art.br"
}

variable "files_key_pair_id" {
  description = "Id da chave pública do CloudFront de `files.` (grupo de chaves da plataforma). Nulo: assinatura de desenvolvimento."
  type        = string
  default     = null
}

variable "files_private_key_param" {
  description = "Parâmetro SSM (SecureString) com a chave privada PKCS#8 que assina as URLs de `files.`."
  type        = string
  default     = null
}

variable "enable_carrier_hooks" {
  description = "Cria a entrada `hooks.` (API regional com mTLS) da transportadora."
  type        = bool
  default     = false
}

variable "hooks_domain_name" {
  description = "Domínio da entrada de webhooks."
  type        = string
  default     = "hooks.saci.art.br"
}

variable "hooks_certificate_arn" {
  description = "Certificado ACM regional de `hooks_domain_name`."
  type        = string
  default     = null
}

variable "hooks_truststore_uri" {
  description = "Truststore (PEM no S3) com a CA da transportadora, para o mTLS."
  type        = string
  default     = null
}

locals {
  files_bucket = data.aws_ssm_parameter.files_bucket.value
  env = merge(
    {
      FILES_BUCKET          = local.files_bucket
      FILES_BASE_URL        = var.files_base_url
      CARRIER_SECRETS_PARAM = "/saci/${var.environment}/commerce/carrier-secrets"
    },
    var.files_key_pair_id == null ? {} : { FILES_KEY_PAIR_ID = var.files_key_pair_id, FILES_PRIVATE_KEY_PARAM = var.files_private_key_param },
  )
  files_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      { Sid = "Labels", Effect = "Allow", Action = ["s3:PutObject", "s3:GetObject"], Resource = "arn:aws:s3:::${local.files_bucket}/labels/*" },
      { Sid = "FilesKey", Effect = "Allow", Action = ["kms:GenerateDataKey", "kms:Decrypt"], Resource = "*", Condition = { StringEquals = { "kms:ViaService" = "s3.${var.region}.amazonaws.com" } } },
      { Sid = "SigningKey", Effect = "Allow", Action = ["ssm:GetParameter"], Resource = "arn:aws:ssm:${var.region}:${data.aws_caller_identity.current.account_id}:parameter/saci/${var.environment}/*" },
    ]
  })
}

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "shipping"
  data_class  = "C2"

  tables = {
    main = { data_class = "C1", stream = true }
  }

  functions = {
    api      = { role = "api", description = "shipping-api: CEP, cotação e etiqueta", tables_write = ["main"], invoke_internal = true, environment = local.env, extra_policy_json = local.files_policy }
    internal = { role = "internal", description = "CEP e cotação para sellers e checkout", tables_read = ["main"], invoke_internal = true, environment = local.env }
    labels   = { role = "labels", description = "Compra a etiqueta na peça pronta e guarda o rastreio", timeout_seconds = 30, tables_write = ["main"], invoke_internal = true, environment = local.env, extra_policy_json = local.files_policy }
    hooks    = { role = "hooks", description = "carrier-webhooks: rastreio da transportadora", tables_write = ["main"], environment = local.env, extra_policy_json = local.files_policy }
    relay    = { role = "relay", description = "Outbox do frete para o barramento", tables_read = ["main"] }
  }

  routes = {
    "GET /v1/shipping/postal-codes/{postal_code}" = { access = "public", function = "api" }
    "POST /v1/shipping/quotes"                    = { access = "public", function = "api" }
    "GET /v1/shipping/me/sales/{sale_id}/label"   = { access = "user", function = "api" }
  }

  subscriptions = {
    labels = { function = "labels", description = "Etiqueta e rastreio", event_types = ["orders.sale.ready.v1", "orders.sale.shipped.v1"] }
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

# --- Entrada `hooks.` da transportadora (mTLS) -----------------------------------------------

resource "aws_api_gateway_rest_api" "hooks" {
  count                        = var.enable_carrier_hooks ? 1 : 0
  name                         = "saci-${var.environment}-shipping-hooks"
  description                  = "Webhooks da transportadora (mTLS, sem endpoint padrão)"
  disable_execute_api_endpoint = true
  endpoint_configuration {
    types = ["REGIONAL"]
  }
}

resource "aws_api_gateway_resource" "hooks_carrier" {
  count       = var.enable_carrier_hooks ? 1 : 0
  rest_api_id = aws_api_gateway_rest_api.hooks[0].id
  parent_id   = aws_api_gateway_rest_api.hooks[0].root_resource_id
  path_part   = "carrier"
}

resource "aws_api_gateway_resource" "hooks_provider" {
  count       = var.enable_carrier_hooks ? 1 : 0
  rest_api_id = aws_api_gateway_rest_api.hooks[0].id
  parent_id   = aws_api_gateway_resource.hooks_carrier[0].id
  path_part   = "{provider}"
}

resource "aws_api_gateway_method" "hooks" {
  count              = var.enable_carrier_hooks ? 1 : 0
  rest_api_id        = aws_api_gateway_rest_api.hooks[0].id
  resource_id        = aws_api_gateway_resource.hooks_provider[0].id
  http_method        = "POST"
  authorization      = "NONE"
  request_parameters = { "method.request.path.provider" = true }
}

resource "aws_api_gateway_integration" "hooks" {
  count                   = var.enable_carrier_hooks ? 1 : 0
  rest_api_id             = aws_api_gateway_rest_api.hooks[0].id
  resource_id             = aws_api_gateway_resource.hooks_provider[0].id
  http_method             = aws_api_gateway_method.hooks[0].http_method
  type                    = "AWS_PROXY"
  integration_http_method = "POST"
  uri                     = "arn:aws:apigateway:${var.region}:lambda:path/2015-03-31/functions/arn:aws:lambda:${var.region}:${data.aws_caller_identity.current.account_id}:function:${module.domain.function_names["hooks"]}:live/invocations"
}

resource "aws_api_gateway_deployment" "hooks" {
  count       = var.enable_carrier_hooks ? 1 : 0
  rest_api_id = aws_api_gateway_rest_api.hooks[0].id
  triggers    = { redeployment = sha1(jsonencode([aws_api_gateway_integration.hooks[0].uri])) }
  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_api_gateway_stage" "hooks" {
  count         = var.enable_carrier_hooks ? 1 : 0
  rest_api_id   = aws_api_gateway_rest_api.hooks[0].id
  deployment_id = aws_api_gateway_deployment.hooks[0].id
  stage_name    = "live"
}

resource "aws_api_gateway_domain_name" "hooks" {
  count                    = var.enable_carrier_hooks ? 1 : 0
  domain_name              = var.hooks_domain_name
  regional_certificate_arn = var.hooks_certificate_arn
  security_policy          = "TLS_1_2"
  endpoint_configuration {
    types = ["REGIONAL"]
  }
  mutual_tls_authentication {
    truststore_uri = var.hooks_truststore_uri
  }
}

resource "aws_api_gateway_base_path_mapping" "hooks" {
  count       = var.enable_carrier_hooks ? 1 : 0
  api_id      = aws_api_gateway_rest_api.hooks[0].id
  stage_name  = aws_api_gateway_stage.hooks[0].stage_name
  domain_name = aws_api_gateway_domain_name.hooks[0].domain_name
}

resource "aws_lambda_permission" "hooks" {
  count         = var.enable_carrier_hooks ? 1 : 0
  statement_id  = "carrier-hooks"
  action        = "lambda:InvokeFunction"
  function_name = module.domain.function_names["hooks"]
  qualifier     = "live"
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_api_gateway_rest_api.hooks[0].execution_arn}/*/POST/carrier/*"
}

output "functions" {
  description = "Funções implantadas."
  value       = module.domain.function_names
}

output "deployment_trigger" {
  description = "Mudou: reimplante o estágio do API Gateway."
  value       = module.domain.deployment_trigger
}
