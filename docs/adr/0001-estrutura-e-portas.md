# ADR 0001: Estrutura por domínio e núcleo sem nuvem

- **Situação:** aceita
- **Data:** 2026-11-15

## Contexto

A diretiva do repositório pede organização agnóstica de tecnologia, crates Rust isolados do ambiente de nuvem e reutilizáveis em qualquer ambiente, e a proposta (9.1) diz que a portabilidade vem do código de domínio com adaptadores por provedor. A plataforma (`saci-boiuna`, ADR 0001 de lá) já organiza por capacidade e põe o provedor só em adaptadores e *features*.

## Decisão

- A raiz é `domains/<domínio>/<serviço>/` (seção 9.5 e 9.7), mais `crates/` para o comum do repositório, `infra/` para módulos Terraform compartilhados e `local-env/`.
- Cada serviço é um crate cujo núcleo depende só do `saci-commerce-kit` e das bibliotecas da plataforma. O acesso ao mundo de fora são **portas** (*traits* usadas como objeto, `Arc<dyn …>`): `DocumentStore`, `ObjectStore`, `UploadSigner`, `UrlSigner`, `InternalClient`, `TokenSigner`, `GrantStore`, `Clock`.
- Os adaptadores AWS (DynamoDB, S3, KMS, SSM, CloudFront, Lambda) ficam no módulo `aws` do kit, atrás da *feature* `aws`; os de memória servem aos testes e ao servidor de desenvolvimento.
- HTTP sem framework: `Request`/`Response` do kit e roteamento por *slice patterns*. O adaptador da Lambda e o servidor de desenvolvimento convertem de e para eles.

## Consequências

- Os testes e o servidor de desenvolvimento rodam o código real sem nuvem.
- Trocar de provedor é escrever adaptadores e `infra/<nuvem>`; regras e rotas não mudam.
- O custo é um nível de indireção (futuros em caixa), desprezível perto de uma ida ao banco.
