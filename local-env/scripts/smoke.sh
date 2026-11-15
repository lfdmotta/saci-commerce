#!/usr/bin/env bash
# Teste de fumaça do saci-commerce no LocalStack: API Gateway → autorizador da
# plataforma → funções dos domínios → DynamoDB, eventos e chamadas internas.
# Os tokens são emitidos com as chaves de desenvolvimento da plataforma
# (saci-devtools mint), para as contas de exemplo das sementes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PLATFORM="${ROOT}/../saci-boiuna"
SECRETS="${PLATFORM}/local-env/.secrets"
DEVTOOLS="${PLATFORM}/target/release/saci-devtools"
ssm() { aws ssm get-parameter --name "$1" --query Parameter.Value --output text; }
API_ID="$(ssm /saci/local/gateway/rest-api-id)"
BASE="http://${API_ID}.execute-api.localhost.localstack.cloud:4566/live"
FAILURES=0

check() {
  if [[ "$3" == "$2" ]]; then printf '  \033[32mok\033[0m   %s\n' "$1"
  else printf '  \033[31mFALHA\033[0m %s: esperado %s, veio %s\n' "$1" "$2" "$3"; FAILURES=$((FAILURES + 1)); fi
}
status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
account() { jq -r ".accounts[$1].id" "${ROOT}/local-env/sandbox/demo-accounts.json"; }
mint() {
  "${DEVTOOLS}" mint --key "${SECRETS}/local-user.pk8" --kid local-user --iss https://auth.local.saci \
    --aud saci-local --sub "$1" --kind user "${@:2}"
}

BUYER="$(mint "$(account 0)")"
ARTISAN="$(mint "$(account 1)")"

echo "Vitrine aberta"
check "categorias" 200 "$(status "${BASE}/v1/catalog/categories")"
check "busca" 200 "$(status "${BASE}/v1/catalog/search?q=barro")"
check "diretório" 200 "$(status "${BASE}/v1/catalog/artisans")"
check "vitrine pública" 200 "$(status "${BASE}/v1/sellers/storefronts/barro-vivo")"
check "peças da vitrine" 200 "$(status "${BASE}/v1/catalog/storefronts/barro-vivo/listings")"
check "avaliações" 200 "$(status "${BASE}/v1/reviews/storefronts/barro-vivo")"
check "CEP" 200 "$(status "${BASE}/v1/shipping/postal-codes/60175047")"
check "rota inexistente → 404" 404 "$(status "${BASE}/v1/catalog/nada")"

echo "Conta"
check "perfil sem token → 401" 401 "$(status "${BASE}/v1/customers/me")"
check "perfil com token" 200 "$(status -H "Authorization: Bearer ${BUYER}" "${BASE}/v1/customers/me")"
check "compras" 200 "$(status -H "Authorization: Bearer ${BUYER}" "${BASE}/v1/orders/me/purchases")"
check "comprador sem vitrine → 404" 404 "$(status -H "Authorization: Bearer ${BUYER}" "${BASE}/v1/sellers/me/workspace")"

echo "Painel (papel lido da tabela authz pelo autorizador)"
check "painel da dona" 200 "$(status -H "Authorization: Bearer ${ARTISAN}" "${BASE}/v1/sellers/me/workspace")"
check "vendas" 200 "$(status -H "Authorization: Bearer ${ARTISAN}" "${BASE}/v1/orders/me/sales")"
check "anúncios" 200 "$(status -H "Authorization: Bearer ${ARTISAN}" "${BASE}/v1/catalog/me/listings")"
check "chamados" 200 "$(status -H "Authorization: Bearer ${ARTISAN}" "${BASE}/v1/support/me/tickets")"

echo "Compra (saga com chamadas internas entre funções)"
LISTING="$(curl -s "${BASE}/v1/catalog/listings?limit=48" | jq -c '[.items[] | select(.stock >= 2)][0]')"
ID="$(jq -r .id <<<"${LISTING}")"; ARTISAN_ID="$(jq -r .artisan.id <<<"${LISTING}")"; PRICE="$(jq -r .price.amount <<<"${LISTING}")"
QUOTE="$(curl -s -X POST -H 'content-type: application/json' "${BASE}/v1/shipping/quotes" \
  -d "{\"postal_code\":\"60175047\",\"storefronts\":[{\"artisan_id\":\"${ARTISAN_ID}\",\"items\":[{\"listing_id\":\"${ID}\",\"quantity\":1}]}]}" | jq -r '.items[0].price.amount')"
BODY="{\"items\":[{\"listing_id\":\"${ID}\",\"quantity\":1}],\"shipping_address\":{\"kind\":\"brasil\",\"recipient\":\"Marina\",\"postal_code\":\"60175047\",\"street\":\"Rua A\",\"number\":\"10\",\"city\":\"Fortaleza\",\"state\":\"CE\"},\"payment\":{\"method\":\"pix\"},\"expected_total\":{\"amount\":$((PRICE + QUOTE)),\"currency\":\"BRL\"}}"
check "checkout Pix" 201 "$(status -X POST -H "Authorization: Bearer ${BUYER}" -H 'content-type: application/json' -H "Idempotency-Key: $(cat /proc/sys/kernel/random/uuid)" -d "${BODY}" "${BASE}/v1/checkout")"

[[ ${FAILURES} -eq 0 ]] && printf '\n\033[32mTudo certo.\033[0m\n' || { printf '\n\033[31m%d falha(s)\033[0m\n' "${FAILURES}"; exit 1; }
