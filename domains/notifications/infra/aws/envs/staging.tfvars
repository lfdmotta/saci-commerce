environment              = "staging"
permissions_boundary_arn = "arn:aws:iam::<ACCOUNT_ID>:policy/saci-staging-service-boundary"
log_retention_days       = 90
# Até saci-identity e saci-payments existirem (plano de ação, etapas 2 e 3).
use_sandbox   = true
seed_fixtures = true
