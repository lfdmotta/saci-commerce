/**
 * # catalog
 *
 * Domínio `catalog` (proposta 5.4): `catalog-api` e `search-indexer`, cada
 * papel na sua função (ADR 0003), atrás de `/v1/catalog`.
 *
 * | Função | Para | Escala |
 * | --- | --- | --- |
 * | `api` | ficha, atributos, anúncios do artesão | escrita e leitura por id |
 * | `search` | categorias, listas, busca, diretório, peças da vitrine | a mais acessada; lê a projeção em memória |
 * | `internal` | estoque (escrita condicional) e leituras para outros domínios | chamada pelo `checkout` na compra |
 * | `search-internal` | reindexar | rara |
 * | `indexer` | projeção de busca por eventos do catálogo | por evento |
 * | `artisans` | perfil do diretório por eventos das vitrines | por evento |
 * | `relay` | *outbox* → barramento | por stream |
 * | `migrate` | sementes e reindexação | uma vez por implantação |
 *
 * Tabelas: `saci-<env>-catalog` (C1: estoque e reservas; o resto é público) e
 * `saci-<env>-catalog-search` (C0, projeção que pode ser refeita do zero).
 */

module "domain" {
  source = "../../../../infra/aws/modules/domain"

  environment = var.environment
  region      = var.region
  domain      = "catalog"
  data_class  = "C1"

  tables = {
    main   = { data_class = "C1", stream = true }
    search = { data_class = "C0" }
  }

  functions = {
    api             = { role = "api", description = "catalog-api: ficha, atributos e anúncios do artesão", tables_write = ["main"], invoke_internal = true }
    search          = { role = "search", description = "search-indexer: busca, listas, diretório e vitrine", memory_mb = 512, tables_read = ["search"], environment = { SEARCH_SNAPSHOT_SECS = "30" } }
    internal        = { role = "internal", description = "Estoque e leituras do catálogo para outros domínios", tables_write = ["main"], invoke_internal = true }
    search-internal = { role = "search-internal", description = "Reindexação da busca", timeout_seconds = 300, memory_mb = 512, tables_read = ["main"], tables_write = ["search"] }
    indexer         = { role = "indexer", description = "Projeção de busca a partir dos eventos do catálogo", timeout_seconds = 30, tables_read = ["main"], tables_write = ["search"] }
    artisans        = { role = "artisans", description = "Perfis do diretório a partir das vitrines", timeout_seconds = 30, tables_write = ["main"], invoke_internal = true }
    relay           = { role = "relay", description = "Outbox do catálogo para o barramento", tables_read = ["main"] }
    migrate         = { role = "migrate", description = "Migrações de dados do catálogo", timeout_seconds = 300, tables_write = ["main"], invoke_internal = true }
  }

  routes = {
    "GET /v1/catalog/categories"                       = { access = "public", function = "search" }
    "GET /v1/catalog/listings"                         = { access = "public", function = "search" }
    "GET /v1/catalog/search"                           = { access = "public", function = "search" }
    "GET /v1/catalog/artisans"                         = { access = "public", function = "search" }
    "GET /v1/catalog/artisans/{artisan_id}"            = { access = "public", function = "search" }
    "GET /v1/catalog/storefronts/{url_name}/listings"  = { access = "public", function = "search" }
    "GET /v1/catalog/listings/{listing_id}"            = { access = "public", function = "api" }
    "GET /v1/catalog/listing-attributes"               = { access = "public", function = "api" }
    "GET /v1/catalog/me/listings"                      = { access = "user", function = "api" }
    "POST /v1/catalog/me/listings"                     = { access = "user", function = "api" }
    "GET /v1/catalog/me/listings/{listing_id}"         = { access = "user", function = "api" }
    "PUT /v1/catalog/me/listings/{listing_id}"         = { access = "user", function = "api" }
    "DELETE /v1/catalog/me/listings/{listing_id}"      = { access = "user", function = "api" }
    "POST /v1/catalog/me/listings/{listing_id}/pause"  = { access = "user", function = "api" }
    "POST /v1/catalog/me/listings/{listing_id}/resume" = { access = "user", function = "api" }
  }

  subscriptions = {
    indexer = {
      function    = "indexer"
      description = "Projeção de busca"
      event_types = ["catalog.listing.published.v1", "catalog.listing.updated.v1", "catalog.listing.paused.v1", "catalog.listing.withdrawn.v1", "catalog.stock.changed.v1"]
    }
    artisans = {
      function    = "artisans"
      description = "Perfis do diretório"
      event_types = ["sellers.storefront.opened.v1", "sellers.storefront.updated.v1"]
    }
  }

  internal_targets = local.internal_targets
  sandbox_table    = local.sandbox_table
  seed_fixtures    = var.seed_fixtures

  lambda_zip               = var.lambda_zip
  lambda_architecture      = var.lambda_architecture
  lambda_extra_environment = var.lambda_extra_environment
  permissions_boundary_arn = var.permissions_boundary_arn
  log_retention_days       = var.log_retention_days
  alarm_topic_arn          = var.alarm_topic_arn
}

output "functions" {
  description = "Funções implantadas."
  value       = module.domain.function_names
}

output "deployment_trigger" {
  description = "Mudou: reimplante o estágio do API Gateway."
  value       = module.domain.deployment_trigger
}
