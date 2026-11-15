/**
 * # shared
 *
 * O que os domínios do `saci-commerce` dividem num ambiente. Aplicado antes de
 * qualquer domínio (`local-env/scripts/deploy.sh` e o workflow de implantação).
 *
 * - **Chave dos cursores** (`/saci/<env>/commerce/cursor-key`, `SecureString`):
 *   assina os cursores de página e as digitais (e-mail das novidades, token
 *   de aparelho). Gerada aqui, nunca escrita em código nem em variável de função.
 * - **Tabela do sandbox** (`saci-<env>-sandbox`), só fora de produção: o
 *   estado simulado dos domínios de outros repositórios ainda não criados
 *   (cofre, sessões, KYC, cobranças), dividido por todas as funções.
 * - **Chave do guest token** do ambiente local, gravada pelo `deploy.sh`
 *   (`/saci/local/commerce/guest-key`), porque o LocalStack não tem a chave
 *   assimétrica do KMS que a nuvem usa.
 */

terraform {
  required_version = ">= 1.10.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.73"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.7"
    }
  }

  backend "s3" {}
}

locals {
  is_local = var.environment == "local"
  name     = "saci-${var.environment}-commerce"
}

provider "aws" {
  region                      = var.region
  skip_credentials_validation = local.is_local
  s3_use_path_style           = local.is_local
  default_tags {
    tags = {
      "saci:project"     = "saci"
      "saci:repository"  = "saci-commerce"
      "saci:component"   = "shared"
      "saci:environment" = var.environment
      "saci:managed-by"  = "terraform"
    }
  }
}

resource "random_password" "cursor_key" {
  length  = 48
  special = false
}

resource "aws_ssm_parameter" "cursor_key" {
  name        = "/saci/${var.environment}/commerce/cursor-key"
  description = "Chave HMAC dos cursores e digitais do saci-commerce."
  type        = "SecureString"
  value       = random_password.cursor_key.result
}

# Segredo da assinatura dos webhooks da transportadora (`provedor=segredo`). Troque
# pelo segredo combinado com a transportadora quando houver contrato.
resource "random_password" "carrier_sandbox" {
  length  = 40
  special = false
}

resource "aws_ssm_parameter" "carrier_secrets" {
  name        = "/saci/${var.environment}/commerce/carrier-secrets"
  description = "Segredos HMAC dos webhooks das transportadoras (provedor=segredo,...)."
  type        = "SecureString"
  value       = "sandbox=${random_password.carrier_sandbox.result}"

  lifecycle {
    ignore_changes = [value]
  }
}

module "sandbox_table" {
  source = "../../../../../saci-boiuna/libs/terraform/aws/dynamodb-table"
  count  = var.environment == "prod" ? 0 : 1

  name                   = "saci-${var.environment}-sandbox"
  data_class             = "C2"
  point_in_time_recovery = false
  deletion_protection    = false
  tags                   = { "saci:service" = "sandbox" }
}

resource "aws_ssm_parameter" "sandbox_table" {
  count = var.environment == "prod" ? 0 : 1
  name  = "/saci/${var.environment}/commerce/sandbox-table"
  type  = "String"
  value = module.sandbox_table[0].name
}

output "sandbox_table" {
  description = "Tabela do sandbox (nula em produção)."
  value       = try(module.sandbox_table[0].name, null)
}

output "cursor_key_parameter" {
  description = "Parâmetro da chave dos cursores."
  value       = aws_ssm_parameter.cursor_key.name
}
