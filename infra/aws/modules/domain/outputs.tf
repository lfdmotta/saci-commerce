output "function_names" {
  description = "Funções implantadas, por papel."
  value       = { for k, f in module.function : k => f.function_name }
}

output "function_role_names" {
  description = "Papéis IAM das funções (para permissões adicionadas no stack do domínio)."
  value       = { for k, f in module.function : k => f.role_name }
}

output "table_names" {
  description = "Tabelas, por chave."
  value       = local.table_names
}

output "table_arns" {
  description = "ARNs das tabelas, por chave."
  value       = local.table_arns
}

output "key_arn" {
  description = "Chave KMS do domínio (nula no local)."
  value       = local.key_arn
}

output "deployment_trigger" {
  description = "Mudou: reimplante o estágio do API Gateway."
  value       = try(module.routes[0].deployment_trigger, "")
}
