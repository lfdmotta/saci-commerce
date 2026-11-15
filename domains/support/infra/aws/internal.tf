# Alvos de chamada interna (ADR 0004): os domínios deste repositório pelo
# nome da função `*-internal`; os de outros repositórios pelo sandbox (fora de
# produção) ou pelas funções informadas em `external_functions`.
# Arquivo igual em todos os stacks de domínio.

locals {
  commerce_targets = {
    catalog  = "saci-${var.environment}-catalog-internal"
    search   = "saci-${var.environment}-catalog-search-internal"
    sellers  = "saci-${var.environment}-sellers-internal"
    shipping = "saci-${var.environment}-shipping-internal"
    media    = "saci-${var.environment}-media-internal"
    orders   = "saci-${var.environment}-orders-internal"
    support  = "saci-${var.environment}-support-internal"
  }

  external_targets = {
    for t in ["identity", "kyc", "payments", "finance", "assistant"] :
    t => var.use_sandbox ? "sandbox" : lookup(var.external_functions, t, "saci-${var.environment}-${t}-internal")
  }

  internal_targets = merge(local.commerce_targets, local.external_targets)
}

data "aws_ssm_parameter" "sandbox_table" {
  count = var.use_sandbox ? 1 : 0
  name  = "/saci/${var.environment}/commerce/sandbox-table"
}

locals {
  sandbox_table = var.use_sandbox ? data.aws_ssm_parameter.sandbox_table[0].value : null
}
