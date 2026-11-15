output "root_resource_id" {
  description = "Id do recurso `/v1/<domínio>`."
  value       = aws_api_gateway_resource.root.id
}

output "deployment_trigger" {
  description = "Hash das rotas. Mudou: o estágio do API Gateway precisa de nova implantação."
  value = sha1(jsonencode([
    for k in sort(keys(aws_api_gateway_integration.this)) : [
      k, aws_api_gateway_method.this[k].id, aws_api_gateway_method.this[k].authorizer_id, aws_api_gateway_integration.this[k].uri,
    ]
  ]))
}
