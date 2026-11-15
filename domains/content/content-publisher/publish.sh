#!/usr/bin/env bash
# content-publisher: valida e publica os layouts (proposta 5.14).
#
#   PR aprovado → validação de esquema e de endereços (saci-contracts) →
#   balde do CDN versionado → invalidação do CloudFront.
#
# Uso: publish.sh <ambiente>   (dev | staging | prod; no CI, com o papel OIDC do content-publisher)
#
# Lê do SSM da plataforma: /saci/<env>/edge/bucket/cdn e /saci/<env>/edge/distribution/cdn.
# O host do CDN nos JSON é o de produção (o que o validador aceita); aqui ele é
# trocado pelo do ambiente (cdn-dev., cdn-hml.).
set -euo pipefail

ENVIRONMENT="${1:?informe o ambiente (dev, staging, prod)}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
LAYOUTS="${ROOT}/domains/content/layouts"
CONTRACTS="${CONTRACTS_DIR:-${ROOT}/../saci-contracts}"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

case "${ENVIRONMENT}" in
  prod) HOST="cdn.saci.art.br" ;;
  staging) HOST="cdn-hml.saci.art.br" ;;
  dev) HOST="cdn-dev.saci.art.br" ;;
  *) echo "ambiente inválido: ${ENVIRONMENT}" >&2; exit 1 ;;
esac

echo "==> Validando os layouts com ${CONTRACTS}"
for f in "${LAYOUTS}"/*.json; do
  node "${CONTRACTS}/tooling/validate-layout.mjs" "${f}"
done

echo "==> Preparando para ${HOST}"
mkdir -p "${WORK}/media"
for f in "${LAYOUTS}"/*.json; do
  sed "s#https://cdn.saci.art.br/layouts/#https://${HOST}/layouts/#g" "${f}" > "${WORK}/$(basename "${f}")"
done
cp "${LAYOUTS}"/media/* "${WORK}/media/" 2>/dev/null || true

param() { aws ssm get-parameter --name "/saci/${ENVIRONMENT}/edge/$1" --query Parameter.Value --output text; }
BUCKET="$(param bucket/cdn)"

echo "==> Publicando em s3://${BUCKET}/layouts"
# Imagens: imutáveis por nome. JSON: TTL curto (a interface também respeita `ttlSeconds`).
aws s3 sync "${WORK}/media" "s3://${BUCKET}/layouts/media" --cache-control "public, max-age=86400" --content-type image/jpeg
aws s3 sync "${WORK}" "s3://${BUCKET}/layouts" --exclude "media/*" --cache-control "public, max-age=60" --content-type application/json

if DIST="$(param distribution/cdn 2>/dev/null)"; then
  echo "==> Invalidando /layouts/* em ${DIST}"
  aws cloudfront create-invalidation --distribution-id "${DIST}" --paths "/layouts/*" >/dev/null
fi
echo "Pronto."
