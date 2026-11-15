environment              = "prod"
permissions_boundary_arn = "arn:aws:iam::<ACCOUNT_ID>:policy/saci-prod-service-boundary"
log_retention_days       = 365
use_sandbox              = false
seed_fixtures            = false

# Funções internas dos outros repositórios (preencha quando existirem).
external_functions = {
  identity  = "saci-prod-vault-internal"
  kyc       = "saci-prod-kyc-gateway"
  payments  = "saci-prod-payments-internal"
  finance   = "saci-prod-finance-internal"
  assistant = "saci-prod-assistant-internal"
}
