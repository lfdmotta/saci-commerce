variable "environment" {
  description = "local, dev, staging ou prod."
  type        = string

  validation {
    condition     = contains(["local", "dev", "staging", "prod"], var.environment)
    error_message = "environment deve ser local, dev, staging ou prod."
  }
}

variable "region" {
  description = "Região."
  type        = string
  default     = "sa-east-1"
}

variable "domain" {
  description = "Domínio (`catalog`, `orders`...): prefixo dos recursos (`saci-<env>-<domínio>`) e raiz `/v1/<domínio>`."
  type        = string
}

variable "data_class" {
  description = "Maior classe de dado do domínio (C0 a C3), etiqueta da chave KMS."
  type        = string
}

variable "tables" {
  description = "Tabelas: chave `main` vira `saci-<env>-<domínio>`; outras, `saci-<env>-<domínio>-<chave>`. `stream` liga o outbox."
  type = map(object({
    data_class = string
    stream     = optional(bool, false)
  }))
  default = {}
}

variable "functions" {
  description = <<-EOT
    Funções do domínio, todas do mesmo ZIP: `role` vira `SACI_FUNCTION`. `tables_read` e
    `tables_write` são chaves de `tables` ou ARNs; `invoke_internal` libera as chamadas
    internas declaradas em `internal_targets`.
  EOT
  type = map(object({
    role                    = string
    description             = string
    memory_mb               = optional(number, 256)
    timeout_seconds         = optional(number, 10)
    environment             = optional(map(string), {})
    tables_read             = optional(list(string), [])
    tables_write            = optional(list(string), [])
    invoke_internal         = optional(bool, false)
    extra_policy_json       = optional(string)
    reserved_concurrency    = optional(number, -1)
    provisioned_concurrency = optional(number, 0)
  }))
}

variable "routes" {
  description = "Rotas `\"MÉTODO /v1/<domínio>/...\" = { access, function }` (ver `../routes`)."
  type = map(object({
    access   = string
    function = string
  }))
  default = {}
}

variable "subscriptions" {
  description = "Assinaturas de eventos: `{ function, event_types, description, batch_size }`."
  type = map(object({
    function    = string
    event_types = list(string)
    description = optional(string, "")
    batch_size  = optional(number, 10)
  }))
  default = {}
}

variable "schedules" {
  description = "Agendamentos: `{ function, expression }` (ex.: `rate(5 minutes)`)."
  type = map(object({
    function   = string
    expression = string
  }))
  default = {}
}

variable "internal_targets" {
  description = "Alvos de chamada interna → nome da função (ou `sandbox`, fora de produção)."
  type        = map(string)
  default     = {}
}

variable "sandbox_table" {
  description = "Tabela do sandbox (estado de identity, payments... simulados). Nula em produção."
  type        = string
  default     = null
}

variable "seed_fixtures" {
  description = "Aplica as sementes fictícias nas migrações (nunca em produção)."
  type        = bool
  default     = false
}

variable "run_migrations" {
  description = "Invoca a função `migrate` depois da implantação."
  type        = bool
  default     = true
}

variable "lambda_zip" {
  description = "ZIP do binário do domínio (`cargo lambda build --output-format zip --bin saci-<domínio>`)."
  type        = string
}

variable "lambda_architecture" {
  description = "`arm64` na AWS; `x86_64` no LocalStack em máquina x86."
  type        = string
  default     = "arm64"
}

variable "lambda_extra_environment" {
  description = "Variáveis extras de todas as funções (no local: `AWS_ENDPOINT_URL`)."
  type        = map(string)
  default     = {}
}

variable "permissions_boundary_arn" {
  description = "Limite de permissões exigido pelo papel de implantação. Nulo no local."
  type        = string
  default     = null
}

variable "log_retention_days" {
  description = "Retenção dos logs."
  type        = number
  default     = 30
}

variable "deletion_protection" {
  description = "Protege as tabelas contra exclusão (sempre desligado no local)."
  type        = bool
  default     = true
}

variable "alarm_topic_arn" {
  description = "Tópico SNS de alertas (DLQ com mensagem). Nulo não cria alarme."
  type        = string
  default     = null
}
