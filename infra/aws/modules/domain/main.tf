/**
 * # domain
 *
 * Tudo o que um domínio do `saci-commerce` implanta, a partir de uma descrição
 * curta no stack do domínio (`domains/<domínio>/infra/aws`):
 *
 * - **chave KMS** do domínio, com a classe de dado (fora do local);
 * - **tabelas** DynamoDB (`pk`/`sk`, sob demanda, PITR, TTL, Streams quando há *outbox*);
 * - **funções** Lambda (uma por papel, todas do mesmo ZIP, `SACI_FUNCTION`
 *   diferente e **papel IAM próprio, só com o que cada uma precisa**, ADR 0003);
 * - **rotas** em `/v1/<domínio>` no API Gateway da plataforma (`../routes`);
 * - **assinaturas** de eventos: regra no barramento → fila → função, com DLQ e
 *   falha parcial por mensagem;
 * - **retransmissão** do *outbox*: stream da tabela → função `relay` → barramento;
 * - **chamadas internas** (ADR 0004): `INTERNAL_<ALVO>_FUNCTION` e
 *   `lambda:InvokeFunction` só nos alvos declarados;
 * - **agendamentos** (varreduras) e a **migração** de dados depois de cada implantação.
 *
 * Usa os módulos da plataforma (`saci-boiuna/libs/terraform/aws`) e lê o
 * contrato da plataforma no SSM (`/saci/<env>/...`), nunca o estado de outro stack.
 */

locals {
  is_local = var.environment == "local"
  prefix   = "saci-${var.environment}-${var.domain}"

  gateway = { for k, p in data.aws_ssm_parameter.gateway : k => p.value }
  network = { for k, p in data.aws_ssm_parameter.network : k => p.value }
  bus     = { for k, p in data.aws_ssm_parameter.eventbus : k => p.value }

  table_names = { for k, _ in var.tables : k => k == "main" ? local.prefix : "${local.prefix}-${k}" }
  key_arn     = local.is_local ? null : module.key[0].arn

  # Alvo interno → nome da função (ou `sandbox`, só fora de produção).
  internal_env = { for t, f in var.internal_targets : "INTERNAL_${upper(replace(t, "-", "_"))}_FUNCTION" => f }
  invoke_arns = [
    for f in distinct(values(var.internal_targets)) :
    "arn:${data.aws_partition.current.partition}:lambda:${var.region}:${data.aws_caller_identity.current.account_id}:function:${f}:live"
    if f != "sandbox"
  ]

  common_env = merge(
    {
      SACI_ENVIRONMENT      = var.environment
      SACI_CURSOR_KEY_PARAM = "/saci/${var.environment}/commerce/cursor-key"
      EVENT_BUS             = local.bus["name"]
      SACI_SEED_FIXTURES    = tostring(var.seed_fixtures)
    },
    { for k, n in local.table_names : (k == "main" ? "TABLE" : "${upper(replace(k, "-", "_"))}_TABLE") => n },
    var.sandbox_table == null ? {} : { SANDBOX_TABLE = var.sandbox_table },
    local.internal_env,
    var.lambda_extra_environment,
  )

  tags = { "saci:service" = var.domain }
}

data "aws_partition" "current" {}
data "aws_caller_identity" "current" {}

# --- Contrato da plataforma (SSM) ------------------------------------------------------

data "aws_ssm_parameter" "gateway" {
  for_each = length(var.routes) == 0 ? toset([]) : toset(["rest-api-id", "execution-arn", "resource/v1", "authorizer/user", "authorizer/mixed"])
  name     = "/saci/${var.environment}/gateway/${each.key}"
}

data "aws_ssm_parameter" "network" {
  for_each = local.is_local ? toset([]) : toset(["vpc-id", "vpc-cidr", "private-subnet-ids", "gateway-prefix-list-ids"])
  name     = "/saci/${var.environment}/network/${each.key}"
}

data "aws_ssm_parameter" "eventbus" {
  for_each = toset(["name", "arn"])
  name     = "/saci/${var.environment}/eventbus/${each.key}"
}

# --- Chave e tabelas -----------------------------------------------------------------------

module "key" {
  source      = "../../../../../saci-boiuna/libs/terraform/aws/kms-key"
  count       = local.is_local ? 0 : 1
  alias       = local.prefix
  description = "Dados do domínio ${var.domain} (saci-commerce)"
  data_class  = var.data_class
  tags        = local.tags

  extra_policy_json = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "CloudWatchLogs"
      Effect    = "Allow"
      Principal = { Service = "logs.${var.region}.amazonaws.com" }
      Action    = ["kms:Encrypt*", "kms:Decrypt*", "kms:ReEncrypt*", "kms:GenerateDataKey*", "kms:Describe*"]
      Resource  = "*"
      Condition = { ArnLike = { "kms:EncryptionContext:aws:logs:arn" = "arn:${data.aws_partition.current.partition}:logs:${var.region}:${data.aws_caller_identity.current.account_id}:*" } }
    }]
  })
}

module "table" {
  source   = "../../../../../saci-boiuna/libs/terraform/aws/dynamodb-table"
  for_each = var.tables

  name                   = local.table_names[each.key]
  data_class             = each.value.data_class
  kms_key_arn            = local.key_arn
  stream                 = each.value.stream
  point_in_time_recovery = !local.is_local
  deletion_protection    = var.deletion_protection && !local.is_local
  backup                 = !local.is_local && var.environment == "prod"
  tags                   = local.tags
}

# --- Funções -------------------------------------------------------------------------------

data "aws_iam_policy_document" "function" {
  for_each = var.functions

  dynamic "statement" {
    for_each = length(each.value.tables_read) + length(each.value.tables_write) == 0 ? [] : [1]
    content {
      sid       = "TablesRead"
      actions   = ["dynamodb:GetItem", "dynamodb:BatchGetItem", "dynamodb:Query", "dynamodb:ConditionCheckItem"]
      resources = [for t in distinct(concat(each.value.tables_read, each.value.tables_write)) : lookup(local.table_arns, t, t)]
    }
  }

  dynamic "statement" {
    for_each = length(each.value.tables_write) == 0 ? [] : [1]
    content {
      sid       = "TablesWrite"
      actions   = ["dynamodb:PutItem", "dynamodb:UpdateItem", "dynamodb:DeleteItem", "dynamodb:TransactWriteItems"]
      resources = [for t in each.value.tables_write : lookup(local.table_arns, t, t)]
    }
  }

  dynamic "statement" {
    for_each = local.is_local ? [] : [1]
    content {
      sid       = "DomainKey"
      actions   = ["kms:Decrypt", "kms:Encrypt", "kms:GenerateDataKey"]
      resources = [local.key_arn]
    }
  }

  statement {
    sid       = "CursorKey"
    actions   = ["ssm:GetParameter"]
    resources = ["arn:${data.aws_partition.current.partition}:ssm:${var.region}:${data.aws_caller_identity.current.account_id}:parameter/saci/${var.environment}/commerce/*"]
  }

  dynamic "statement" {
    for_each = each.value.invoke_internal && length(local.invoke_arns) > 0 ? [1] : []
    content {
      sid       = "InternalCalls"
      actions   = ["lambda:InvokeFunction"]
      resources = local.invoke_arns
    }
  }

  dynamic "statement" {
    for_each = each.value.invoke_internal && var.sandbox_table != null ? [1] : []
    content {
      sid       = "SandboxTable"
      actions   = ["dynamodb:GetItem", "dynamodb:Query", "dynamodb:PutItem", "dynamodb:DeleteItem", "dynamodb:TransactWriteItems", "dynamodb:ConditionCheckItem"]
      resources = ["arn:${data.aws_partition.current.partition}:dynamodb:${var.region}:${data.aws_caller_identity.current.account_id}:table/${var.sandbox_table}"]
    }
  }

  dynamic "statement" {
    for_each = each.key == "relay" ? [1] : []
    content {
      sid       = "PublishEvents"
      actions   = ["events:PutEvents"]
      resources = [local.bus["arn"]]
    }
  }

  dynamic "statement" {
    for_each = each.key == "relay" ? [1] : []
    content {
      sid       = "ReadStream"
      actions   = ["dynamodb:DescribeStream", "dynamodb:GetRecords", "dynamodb:GetShardIterator", "dynamodb:ListStreams"]
      resources = [for k, t in module.table : "${t.arn}/stream/*" if var.tables[k].stream]
    }
  }

  dynamic "statement" {
    for_each = contains([for s in values(var.subscriptions) : s.function], each.key) ? [1] : []
    content {
      sid       = "ConsumeQueue"
      actions   = ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes", "sqs:ChangeMessageVisibility"]
      resources = [for k, s in module.subscription : s.queue_arn if var.subscriptions[k].function == each.key]
    }
  }

  source_policy_documents = each.value.extra_policy_json == null ? [] : [each.value.extra_policy_json]
}

locals {
  table_arns = { for k, t in module.table : k => t.arn }
}

module "function" {
  source   = "../../../../../saci-boiuna/libs/terraform/aws/rust-lambda"
  for_each = var.functions

  name            = "${local.prefix}-${each.key}"
  description     = each.value.description
  zip_path        = var.lambda_zip
  architecture    = var.lambda_architecture
  memory_mb       = each.value.memory_mb
  timeout_seconds = each.value.timeout_seconds

  environment = merge(local.common_env, { SACI_FUNCTION = each.value.role }, each.value.environment)

  policy_json              = data.aws_iam_policy_document.function[each.key].json
  permissions_boundary_arn = var.permissions_boundary_arn
  log_retention_days       = var.log_retention_days
  logs_kms_key_arn         = local.key_arn
  kms_key_arn              = local.key_arn
  tracing                  = !local.is_local
  reserved_concurrency     = each.value.reserved_concurrency
  provisioned_concurrency  = local.is_local ? 0 : each.value.provisioned_concurrency

  vpc = local.is_local ? null : {
    vpc_id                  = local.network["vpc-id"]
    subnet_ids              = split(",", local.network["private-subnet-ids"])
    egress_cidr             = local.network["vpc-cidr"]
    gateway_prefix_list_ids = split(",", local.network["gateway-prefix-list-ids"])
  }

  tags = local.tags
}

# --- Rotas -----------------------------------------------------------------------------------

module "routes" {
  source = "../routes"
  count  = length(var.routes) == 0 ? 0 : 1

  domain         = var.domain
  rest_api_id    = local.gateway["rest-api-id"]
  execution_arn  = local.gateway["execution-arn"]
  v1_resource_id = local.gateway["resource/v1"]
  authorizer_ids = { user = local.gateway["authorizer/user"], mixed = local.gateway["authorizer/mixed"] }
  functions = {
    for k, f in module.function : k => { name = f.function_name, invoke_arn = f.alias_invoke_arn }
  }
  routes = var.routes
}

# --- Eventos: assinaturas e retransmissão do outbox -----------------------------------------------

module "subscription" {
  source   = "../../../../../saci-boiuna/libs/terraform/aws/event-subscription"
  for_each = var.subscriptions

  name           = "${local.prefix}-${each.key}"
  description    = each.value.description
  event_bus_name = local.bus["name"]
  event_pattern  = { detail-type = each.value.event_types }
  # Maior que o tempo máximo da função consumidora.
  visibility_timeout_seconds = var.functions[each.value.function].timeout_seconds * 6
  alarm_topic_arn            = var.alarm_topic_arn
  tags                       = local.tags
}

resource "aws_lambda_event_source_mapping" "subscription" {
  for_each = var.subscriptions

  event_source_arn                   = module.subscription[each.key].queue_arn
  function_name                      = module.function[each.value.function].alias_arn
  batch_size                         = each.value.batch_size
  maximum_batching_window_in_seconds = local.is_local ? 0 : 1
  function_response_types            = ["ReportBatchItemFailures"]
}

resource "aws_lambda_event_source_mapping" "relay" {
  for_each = { for k, t in var.tables : k => t if t.stream && contains(keys(var.functions), "relay") }

  event_source_arn        = module.table[each.key].stream_arn
  function_name           = module.function["relay"].alias_arn
  starting_position       = "TRIM_HORIZON"
  batch_size              = 25
  function_response_types = ["ReportBatchItemFailures"]
  maximum_retry_attempts  = 10

  # Só os itens de outbox chegam à função (o resto da tabela não custa invocação).
  filter_criteria {
    filter {
      pattern = jsonencode({ eventName = ["INSERT"], dynamodb = { Keys = { pk = { S = [{ prefix = "OUTBOX#" }] } } } })
    }
  }
}

# --- Agendamentos e migração ---------------------------------------------------------------------

resource "aws_cloudwatch_event_rule" "schedule" {
  for_each            = var.schedules
  name                = "${local.prefix}-${each.key}"
  description         = "Agenda de ${each.value.function} (${var.domain})"
  schedule_expression = each.value.expression
  tags                = local.tags
}

resource "aws_cloudwatch_event_target" "schedule" {
  for_each = var.schedules
  rule     = aws_cloudwatch_event_rule.schedule[each.key].name
  arn      = module.function[each.value.function].alias_arn
}

resource "aws_lambda_permission" "schedule" {
  for_each      = var.schedules
  statement_id  = "schedule-${each.key}"
  action        = "lambda:InvokeFunction"
  function_name = module.function[each.value.function].function_name
  qualifier     = "live"
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.schedule[each.key].arn
}

# Migrações de dados depois de cada implantação (idempotentes; ver `src/seed.rs` do serviço).
resource "aws_lambda_invocation" "migrate" {
  count         = contains(keys(var.functions), "migrate") && var.run_migrations ? 1 : 0
  function_name = module.function["migrate"].function_name
  qualifier     = "live"
  input         = jsonencode({ trigger = filebase64sha256(var.lambda_zip) })

  depends_on = [module.function]
}

# --- Contrato para os outros domínios (SSM) ------------------------------------------------------

resource "aws_ssm_parameter" "function" {
  for_each = var.functions
  name     = "/saci/${var.environment}/services/${var.domain}/function/${each.key}"
  type     = "String"
  value    = module.function[each.key].function_name
  tags     = local.tags
}

resource "aws_ssm_parameter" "table" {
  for_each = local.table_names
  name     = "/saci/${var.environment}/services/${var.domain}/table/${each.key}"
  type     = "String"
  value    = each.value
  tags     = local.tags
}
