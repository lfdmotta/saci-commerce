#!/usr/bin/env bash
# Implanta o saci-commerce no LocalStack da plataforma, com os MESMOS stacks
# da nuvem (envs/local.tfvars). Roda dentro da caixa de ferramentas
# (local-env/docker-compose.yml), depois do `task local:deploy` do saci-boiuna.
#
#   1. Confere que a plataforma está no ar (SSM /saci/local/gateway/*).
#   2. Compila os binários dos domínios (cargo lambda, arquitetura da máquina).
#   3. Grava a chave de desenvolvimento do guest token no SSM.
#   4. Aplica: shared → sellers → catalog → media → shipping → orders →
#      checkout → customers → reviews → support → notifications
#      (as migrações de cada domínio chamam os anteriores).
#   5. Publica as rotas no estágio do API Gateway.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PLATFORM="${ROOT}/../saci-boiuna"
DOMAINS=(sellers catalog media shipping orders checkout customers reviews support notifications)

log() { printf '\033[1;34m==> %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31mERRO: %s\033[0m\n' "$*" >&2; exit 1; }
ssm() { aws ssm get-parameter --name "$1" --query Parameter.Value --output text; }

[[ -d "${PLATFORM}" ]] || fail "clone o saci-boiuna ao lado deste repositório (ADR 0002)"
ssm /saci/local/gateway/rest-api-id >/dev/null 2>&1 || fail "a plataforma não está no LocalStack: rode 'task local:up local:deploy' no saci-boiuna"

case "$(uname -m)" in
  aarch64 | arm64) ARCH=arm64; FLAG=--arm64 ;;
  *) ARCH=x86_64; FLAG=--x86-64 ;;
esac

log "Compilando os domínios (${ARCH})"
BINS=()
for d in "${DOMAINS[@]}"; do BINS+=(--bin "saci-${d}"); done
(cd "${ROOT}" && cargo lambda build --release "${FLAG}" --output-format zip "${BINS[@]}")

log "Chave de desenvolvimento do guest token (a mesma que o autorizador local confia)"
GUEST_KEY="${PLATFORM}/local-env/.secrets/local-guest.pk8"
[[ -f "${GUEST_KEY}" ]] || fail "chave ${GUEST_KEY} ausente: rode o deploy local do saci-boiuna"
aws ssm put-parameter --name /saci/local/commerce/guest-key --type SecureString --overwrite \
  --value "$(base64 -w0 "${GUEST_KEY}")" >/dev/null

apply() {
  local dir="$1"; shift
  log "terraform apply: ${dir}"
  (
    cd "${ROOT}/${dir}"
    terraform init -input=false -reconfigure -backend-config=envs/local.s3.tfbackend >/dev/null
    terraform apply -input=false -auto-approve -var-file=envs/local.tfvars "$@"
  )
}

apply infra/aws/stacks/shared
for d in "${DOMAINS[@]}"; do
  extra=(-var "lambda_architecture=${ARCH}" -var "lambda_zip=${ROOT}/target/lambda/saci-${d}/bootstrap.zip")
  if [[ "${d}" == "checkout" ]]; then
    extra+=(-var 'trusted_issuers=["https://auth.local.saci"]' -var "guest_issuer=https://checkout.local.saci" -var "guest_ttl_secs=86400")
  fi
  apply "domains/${d}/infra/aws" "${extra[@]}"
done

log "Publicando as rotas no estágio"
ENVIRONMENT=local "${PLATFORM}/ci/scripts/redeploy-api.sh" "saci-commerce@local"

API_ID="$(ssm /saci/local/gateway/rest-api-id)"
log "Pronto. API local:"
echo "  http://${API_ID}.execute-api.localhost.localstack.cloud:4566/live/v1/catalog/categories"
echo "Teste: task local:smoke"
