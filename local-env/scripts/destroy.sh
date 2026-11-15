#!/usr/bin/env bash
# Remove do LocalStack os recursos do saci-commerce (a plataforma continua).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DOMAINS=(notifications support reviews customers checkout orders shipping media catalog sellers)
for d in "${DOMAINS[@]}" ; do
  dir="${ROOT}/domains/${d}/infra/aws"
  ( cd "${dir}" && terraform init -input=false -reconfigure -backend-config=envs/local.s3.tfbackend >/dev/null \
    && terraform destroy -input=false -auto-approve -var-file=envs/local.tfvars \
       -var "lambda_zip=${ROOT}/target/lambda/saci-${d}/bootstrap.zip" ) || true
done
( cd "${ROOT}/infra/aws/stacks/shared" && terraform init -input=false -reconfigure -backend-config=envs/local.s3.tfbackend >/dev/null \
  && terraform destroy -input=false -auto-approve -var-file=envs/local.tfvars ) || true
