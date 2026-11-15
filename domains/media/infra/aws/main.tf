/**
 * # media
 *
 * Domínio `media` (proposta 5.5, S-14):
 *
 * - **quarentena**: balde privado deste domínio, só `PUT` pré-assinado, CORS
 *   só para o site, objetos apagados em 1 dia;
 * - **verificação**: na nuvem, GuardDuty Malware Protection for S3 no balde da
 *   quarentena; o resultado chega pelo EventBridge e dispara a esteira. No local
 *   (`malware_scan = "none"`), a notificação do S3 dispara a esteira direto;
 * - **publicação**: a esteira grava em `media/` no balde do CDN da plataforma
 *   (SSM `/saci/<env>/edge/bucket/cdn`), servido por `cdn.saci.art.br`.
 */

data "aws_caller_identity" "current" {}

data "aws_ssm_parameter" "cdn_bucket" {
  name = "/saci/${var.environment}/edge/bucket/cdn"
}

variable "malware_scan" {
  description = "`guardduty` (nuvem) ou `none` (LocalStack: a notificação do S3 dispara a esteira)."
  type        = string
  default     = "guardduty"

  validation {
    condition     = contains(["guardduty", "none"], var.malware_scan)
    error_message = "malware_scan deve ser guardduty ou none."
  }
}

variable "site_origins" {
  description = "Origens do site que podem enviar direto para a quarentena (CORS)."
  type        = list(string)
  default     = ["https://saci.art.br"]
}

variable "cdn_base_url" {
  description = "Base pública do CDN."
  type        = string
  default     = "https://cdn.saci.art.br"
}

locals {
  quarantine = "saci-${var.environment}-media-quarantine-${data.aws_caller_identity.current.account_id}"
  cdn_bucket = data.aws_ssm_parameter.cdn_bucket.value
  env = {
    QUARANTINE_BUCKET = local.quarantine
    CDN_BUCKET        = local.cdn_bucket
    CDN_BASE_URL      = var.cdn_base_url
    MALWARE_SCAN      = var.malware_scan
  }
  quarantine_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid      = "Quarantine"
      Effect   = "Allow"
      Action   = ["s3:PutObject", "s3:GetObject", "s3:DeleteObject", "s3:GetObjectTagging"]
      Resource = "${module.quarantine.arn}/quarantine/*"
    }]
  })
  pipeline_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      { Sid = "Quarantine", Effect = "Allow", Action = ["s3:GetObject", "s3:DeleteObject", "s3:GetObjectTagging"], Resource = "${module.quarantine.arn}/quarantine/*" },
      { Sid = "PublishToCdn", Effect = "Allow", Action = ["s3:PutObject"], Resource = "arn:aws:s3:::${local.cdn_bucket}/media/*" },
    ]
  })
}

module "quarantine" {
  source = "../../../../../saci-boiuna/libs/terraform/aws/secure-bucket"

  name            = local.quarantine
  data_class      = "C2"
  kms_key_arn     = module.domain.key_arn
  expiration_days = 1
  versioning      = false
  force_destroy   = var.environment == "local" || var.environment == "dev"
}

resource "aws_s3_bucket_cors_configuration" "quarantine" {
  bucket = module.quarantine.id
  cors_rule {
    allowed_methods = ["PUT"]
    allowed_origins = var.site_origins
    allowed_headers = ["Content-Type", "Content-Length", "x-amz-checksum-sha256", "x-amz-sdk-checksum-algorithm"]
    max_age_seconds = 600
  }
}

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "media"
  data_class  = "C2"

  tables = {
    main = { data_class = "C1", stream = true }
  }

  functions = {
    api      = { role = "api", description = "media-api: envio assinado e estado", tables_write = ["main"], environment = local.env, extra_policy_json = local.quarantine_policy }
    internal = { role = "internal", description = "Estado dos envios para catalog e sellers", tables_read = ["main"], environment = local.env, extra_policy_json = local.quarantine_policy }
    pipeline = { role = "pipeline", description = "media-pipeline: verificação, reescrita sem EXIF, tamanhos, CDN", memory_mb = 1536, timeout_seconds = 60, tables_write = ["main"], environment = local.env, extra_policy_json = local.pipeline_policy, reserved_concurrency = 20 }
    relay    = { role = "relay", description = "Outbox das imagens para o barramento", tables_read = ["main"] }
  }

  routes = {
    "POST /v1/media/me/uploads"            = { access = "user", function = "api" }
    "GET /v1/media/me/uploads/{upload_id}" = { access = "user", function = "api" }
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

# --- Gatilho da esteira ------------------------------------------------------------------

# Local: o objeto novo na quarentena dispara a esteira.
resource "aws_lambda_permission" "s3" {
  count         = var.malware_scan == "none" ? 1 : 0
  statement_id  = "quarantine-object-created"
  action        = "lambda:InvokeFunction"
  function_name = module.domain.function_names["pipeline"]
  qualifier     = "live"
  principal     = "s3.amazonaws.com"
  source_arn    = module.quarantine.arn
}

resource "aws_s3_bucket_notification" "quarantine" {
  count  = var.malware_scan == "none" ? 1 : 0
  bucket = module.quarantine.id
  lambda_function {
    lambda_function_arn = "${local.pipeline_arn}:live"
    events              = ["s3:ObjectCreated:*"]
    filter_prefix       = "quarantine/"
  }
  depends_on = [aws_lambda_permission.s3]
}

locals {
  pipeline_arn = "arn:aws:lambda:${var.region}:${data.aws_caller_identity.current.account_id}:function:${module.domain.function_names["pipeline"]}"
}

# Nuvem: GuardDuty verifica cada objeto e publica o resultado no EventBridge (barramento padrão).
data "aws_iam_policy_document" "guardduty_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["malware-protection-plan.guardduty.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "guardduty" {
  count                = var.malware_scan == "guardduty" ? 1 : 0
  name                 = "saci-${var.environment}-media-malware-scan"
  assume_role_policy   = data.aws_iam_policy_document.guardduty_assume.json
  permissions_boundary = var.permissions_boundary_arn
}

resource "aws_iam_role_policy" "guardduty" {
  count = var.malware_scan == "guardduty" ? 1 : 0
  name  = "scan"
  role  = aws_iam_role.guardduty[0].id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      { Effect = "Allow", Action = ["s3:GetObject", "s3:GetObjectVersion", "s3:PutObjectTagging", "s3:GetObjectTagging", "s3:ListBucket", "s3:GetBucketNotification", "s3:PutBucketNotification"], Resource = [module.quarantine.arn, "${module.quarantine.arn}/*"] },
      { Effect = "Allow", Action = ["kms:Decrypt", "kms:GenerateDataKey"], Resource = module.domain.key_arn },
      { Effect = "Allow", Action = ["events:PutRule", "events:DeleteRule", "events:PutTargets", "events:RemoveTargets", "events:DescribeRule"], Resource = "arn:aws:events:${var.region}:${data.aws_caller_identity.current.account_id}:rule/DO-NOT-DELETE-AmazonGuardDutyMalwareProtectionS3*" },
    ]
  })
}

resource "aws_guardduty_malware_protection_plan" "quarantine" {
  count = var.malware_scan == "guardduty" ? 1 : 0
  role  = aws_iam_role.guardduty[0].arn
  protected_resource {
    s3_bucket {
      bucket_name     = module.quarantine.id
      object_prefixes = ["quarantine/"]
    }
  }
  actions {
    tagging {
      status = "ENABLED"
    }
  }
}

resource "aws_cloudwatch_event_rule" "scan_result" {
  count       = var.malware_scan == "guardduty" ? 1 : 0
  name        = "saci-${var.environment}-media-scan-result"
  description = "Resultado da verificação de malware das imagens enviadas"
  event_pattern = jsonencode({
    source      = ["aws.guardduty"]
    detail-type = ["GuardDuty Malware Protection Object Scan Result"]
    detail      = { s3ObjectDetails = { bucketName = [module.quarantine.id] } }
  })
}

resource "aws_cloudwatch_event_target" "scan_result" {
  count = var.malware_scan == "guardduty" ? 1 : 0
  rule  = aws_cloudwatch_event_rule.scan_result[0].name
  arn   = "${local.pipeline_arn}:live"
}

resource "aws_lambda_permission" "scan_result" {
  count         = var.malware_scan == "guardduty" ? 1 : 0
  statement_id  = "guardduty-scan-result"
  action        = "lambda:InvokeFunction"
  function_name = module.domain.function_names["pipeline"]
  qualifier     = "live"
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.scan_result[0].arn
}

output "functions" {
  description = "Funções implantadas."
  value       = module.domain.function_names
}

output "quarantine_bucket" {
  description = "Balde da quarentena."
  value       = module.quarantine.id
}

output "deployment_trigger" {
  description = "Mudou: reimplante o estágio do API Gateway."
  value       = module.domain.deployment_trigger
}
