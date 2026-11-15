# Variáveis comuns a todos os stacks de domínio do saci-commerce.

variable "environment" {
  description = "local, dev, staging ou prod."
  type        = string
}

variable "region" {
  description = "Região."
  type        = string
  default     = "sa-east-1"
}

variable "lambda_zip" {
  description = "ZIP do binário do domínio. O workflow reutilizável da plataforma passa `-var lambda_zip=...`."
  type        = string
  default     = "../../../../target/lambda/saci-catalog/bootstrap.zip"
}

variable "lambda_architecture" {
  description = "`arm64` na AWS; `x86_64` no LocalStack em máquina x86."
  type        = string
  default     = "arm64"
}

variable "lambda_extra_environment" {
  description = "Variáveis extras das funções (no local: `AWS_ENDPOINT_URL`)."
  type        = map(string)
  default     = {}
}

variable "permissions_boundary_arn" {
  description = "Limite de permissões exigido pelo papel de implantação (`saci-<env>-service-boundary`). Nulo no local."
  type        = string
  default     = null
}

variable "log_retention_days" {
  description = "Retenção dos logs."
  type        = number
  default     = 30
}

variable "use_sandbox" {
  description = <<-EOT
    Atende `identity`, `kyc`, `payments`, `finance` e `assistant` com o sandbox em processo
    (repositórios ainda não criados). Proibido em produção.
  EOT
  type        = bool
  default     = false

  validation {
    condition     = !(var.use_sandbox && var.environment == "prod")
    error_message = "O sandbox não pode ser usado em produção."
  }
}

variable "external_functions" {
  description = "Funções internas dos outros repositórios (saci-identity, saci-payments...), por alvo, quando use_sandbox = false."
  type        = map(string)
  default     = {}
}

variable "seed_fixtures" {
  description = "Aplica as sementes fictícias (vitrines, peças, pedidos de exemplo). Nunca em produção."
  type        = bool
  default     = false

  validation {
    condition     = !(var.seed_fixtures && var.environment == "prod")
    error_message = "Sementes fictícias não entram em produção."
  }
}

variable "alarm_topic_arn" {
  description = "Tópico SNS de alertas (SSM /saci/<env>/observability/alerts-topic-arn). Nulo não cria alarmes de DLQ."
  type        = string
  default     = null
}
