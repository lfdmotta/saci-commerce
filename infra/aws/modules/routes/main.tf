/**
 * # routes
 *
 * Pendura a subárvore `/v1/<domínio>` no API Gateway único da plataforma
 * (ADR 0007 do saci-boiuna), com **uma função por rota**.
 *
 * O módulo `libs/terraform/aws/api-routes` da plataforma liga todas as rotas a
 * uma só função. Aqui cada domínio tem várias funções atrás da mesma raiz (ex.:
 * `catalog-api` e `catalog-search` em `/v1/catalog`, ADR 0003), e os recursos
 * intermediários são compartilhados, então a árvore inteira do domínio precisa
 * ser criada num só lugar. O resto segue o módulo da plataforma: integração
 * `AWS_PROXY` no alias `live`, autorizador por nível de acesso, parâmetros de
 * caminho obrigatórios e permissão de invocação restrita a cada rota.
 *
 * | `access` | Autorizador |
 * | --- | --- |
 * | `public` (`PUB`, `PUB-L`) | nenhum (WAF na borda) |
 * | `user` (`USER`, `USER+`, `SELLER:*`, `OWNER`) | `user` (cache de 60 s) |
 * | `mixed` (`GUEST`, `CONV`) | `mixed` (sem cache) |
 *
 * Depois do `apply`, o estágio precisa de nova implantação
 * (`ci/scripts/redeploy-api.sh` da plataforma).
 */

locals {
  base_path = "/v1/${var.domain}"

  routes = {
    for key, route in var.routes : key => merge(route, {
      method = split(" ", key)[0]
      path   = split(" ", key)[1]
      # Caminho relativo à raiz do domínio; vazio para a própria raiz (POST /v1/checkout).
      rel = split(" ", key)[1] == local.base_path ? "" : trimprefix(split(" ", key)[1], "${local.base_path}/")
    })
  }

  rel_paths = distinct([for r in local.routes : r.rel if r.rel != ""])
  prefixes = distinct(flatten([
    for p in local.rel_paths : [
      for i in range(1, length(split("/", p)) + 1) : join("/", slice(split("/", p), 0, i))
    ]
  ]))
  by_depth = { for d in range(1, 8) : d => toset([for p in local.prefixes : p if length(split("/", p)) == d]) }

  authorizer_ids = {
    public = null
    user   = var.authorizer_ids.user
    mixed  = var.authorizer_ids.mixed
  }
}

# A raiz do domínio, sob o `/v1` da plataforma.
resource "aws_api_gateway_resource" "root" {
  rest_api_id = var.rest_api_id
  parent_id   = var.v1_resource_id
  path_part   = var.domain
}

# Um bloco por profundidade: um recurso não pode depender de outra instância do mesmo bloco.
resource "aws_api_gateway_resource" "d1" {
  for_each    = local.by_depth[1]
  rest_api_id = var.rest_api_id
  parent_id   = aws_api_gateway_resource.root.id
  path_part   = each.key
}

resource "aws_api_gateway_resource" "d2" {
  for_each    = local.by_depth[2]
  rest_api_id = var.rest_api_id
  parent_id   = aws_api_gateway_resource.d1[dirname(each.key)].id
  path_part   = basename(each.key)
}

resource "aws_api_gateway_resource" "d3" {
  for_each    = local.by_depth[3]
  rest_api_id = var.rest_api_id
  parent_id   = aws_api_gateway_resource.d2[dirname(each.key)].id
  path_part   = basename(each.key)
}

resource "aws_api_gateway_resource" "d4" {
  for_each    = local.by_depth[4]
  rest_api_id = var.rest_api_id
  parent_id   = aws_api_gateway_resource.d3[dirname(each.key)].id
  path_part   = basename(each.key)
}

resource "aws_api_gateway_resource" "d5" {
  for_each    = local.by_depth[5]
  rest_api_id = var.rest_api_id
  parent_id   = aws_api_gateway_resource.d4[dirname(each.key)].id
  path_part   = basename(each.key)
}

resource "aws_api_gateway_resource" "d6" {
  for_each    = local.by_depth[6]
  rest_api_id = var.rest_api_id
  parent_id   = aws_api_gateway_resource.d5[dirname(each.key)].id
  path_part   = basename(each.key)
}

locals {
  resource_ids = merge(
    { "" = aws_api_gateway_resource.root.id },
    { for k, r in aws_api_gateway_resource.d1 : k => r.id },
    { for k, r in aws_api_gateway_resource.d2 : k => r.id },
    { for k, r in aws_api_gateway_resource.d3 : k => r.id },
    { for k, r in aws_api_gateway_resource.d4 : k => r.id },
    { for k, r in aws_api_gateway_resource.d5 : k => r.id },
    { for k, r in aws_api_gateway_resource.d6 : k => r.id },
  )

  path_params = {
    for k, r in local.routes : k => {
      for seg in split("/", r.rel) :
      "method.request.path.${trim(seg, "{}+")}" => true if startswith(seg, "{")
    }
  }
}

resource "aws_api_gateway_method" "this" {
  for_each = local.routes

  rest_api_id        = var.rest_api_id
  resource_id        = local.resource_ids[each.value.rel]
  http_method        = each.value.method
  authorization      = each.value.access == "public" ? "NONE" : "CUSTOM"
  authorizer_id      = local.authorizer_ids[each.value.access]
  request_parameters = local.path_params[each.key]
}

resource "aws_api_gateway_integration" "this" {
  for_each = local.routes

  rest_api_id             = var.rest_api_id
  resource_id             = aws_api_gateway_method.this[each.key].resource_id
  http_method             = aws_api_gateway_method.this[each.key].http_method
  type                    = "AWS_PROXY"
  integration_http_method = "POST"
  uri                     = var.functions[each.value.function].invoke_arn
  timeout_milliseconds    = var.integration_timeout_ms
}

resource "aws_lambda_permission" "this" {
  for_each = local.routes

  statement_id  = "apigw-${substr(sha1(each.key), 0, 12)}"
  action        = "lambda:InvokeFunction"
  function_name = var.functions[each.value.function].name
  qualifier     = "live"
  principal     = "apigateway.amazonaws.com"
  # Só esta rota, deste API, em qualquer estágio.
  source_arn = "${var.execution_arn}/*/${each.value.method}${replace(each.value.path, "/\\{[^}]+\\}/", "*")}"
}
