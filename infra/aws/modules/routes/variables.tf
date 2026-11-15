variable "domain" {
  description = "Domínio, que vira a raiz `/v1/<domínio>`."
  type        = string
}

variable "rest_api_id" {
  description = "Id do API Gateway da plataforma (SSM `/saci/<env>/gateway/rest-api-id`)."
  type        = string
}

variable "execution_arn" {
  description = "ARN de execução do API (SSM `/saci/<env>/gateway/execution-arn`)."
  type        = string
}

variable "v1_resource_id" {
  description = "Id do recurso `/v1` da plataforma (SSM `/saci/<env>/gateway/resource/v1`)."
  type        = string
}

variable "authorizer_ids" {
  description = "Ids dos autorizadores `user` e `mixed` (SSM `/saci/<env>/gateway/authorizer/*`)."
  type = object({
    user  = string
    mixed = string
  })
}

variable "functions" {
  description = "Funções que atendem rotas: nome → `{ name, invoke_arn }` (do alias `live`)."
  type = map(object({
    name       = string
    invoke_arn = string
  }))
}

variable "routes" {
  description = <<-EOT
    Rotas: chave `"MÉTODO /v1/<domínio>/caminho"`; `access` = `public`, `user` ou `mixed`;
    `function` = chave em `functions`.
  EOT
  type = map(object({
    access   = string
    function = string
  }))

  validation {
    condition     = alltrue([for r in values(var.routes) : contains(["public", "user", "mixed"], r.access)])
    error_message = "access deve ser public, user ou mixed."
  }

  validation {
    condition     = alltrue([for k in keys(var.routes) : can(regex("^(GET|POST|PUT|PATCH|DELETE) /v1/", k))])
    error_message = "As chaves devem ter o formato \"MÉTODO /v1/...\"."
  }
}

variable "integration_timeout_ms" {
  description = "Tempo máximo da integração (até 29 000 ms no REST)."
  type        = number
  default     = 10000
}
