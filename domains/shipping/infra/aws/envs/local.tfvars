environment         = "local"
lambda_architecture = "x86_64"
log_retention_days  = 1
use_sandbox         = true
seed_fixtures       = true

lambda_extra_environment = {
  AWS_ENDPOINT_URL = "http://localhost.localstack.cloud:4566"
}
