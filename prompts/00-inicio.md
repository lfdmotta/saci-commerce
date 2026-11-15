Leia o @../../saci-sume/visao-geral.md

Este repositório implementa os serviços de vitrine, anúncios, avaliação de produtos e outros descritos para `saci-commerce` em @../../saci-sume/back-end-proposta.md . O conceito é centralizar serviços e dados que são expostos publicamente para internet para todos que acessem o app Saci ou página da web.

Implemente o conteúdo necessário desse repositório seguindo  @../../saci-sume/back-end-proposta.md e o @../../saci-sume/back-end-plano-de-acao.md. 

# Contexto

Contratos da API: os padrões de comunicação da interface já estão criados em ../../saci-contracts .

A infraestrutura comum, o repositório `saci-boiuna` está em ../../saci-boiuna/. 

# Diretivas

Organize o repositório de forma agnóstica a tecnologia: sem dependência explícita de linguagem de programação ou provedor de nuvem para infraestrutura.

A linguagem adotada para infraestrutura como código é Terraform. Siga as melhores práticas de documentação do código Terraform. Documente todas as decisões necessárias de detalhes do serviço.

O provedor de infraestrutura inicial para todos os serviços é a AWS. Crie todo o código necessário de infraestrutura para ambiente AWS e também para que possa ser executado localmente em PC para testes de todos os serviços e integração completa. Prepare o ambiente para que teste local da interface conectada com o teste local do back-end seja possível.

A linguagem preferencial do back-end é Rust para o tipo de serviço que estamos criando aqui. Quando necessário escrever funções em Rust, prefira código minimalista, com pouca dependência externa, focado em desempenho máximo, ou seja execução rápida em nuvem. Paralelize todas as tarefas que podem ser paralelizáveis em lambdas AWS (funções) separadas. Isole as `crates` Rust necessárias de ambiente de nuvem, de modo que possam ser reutilizadas em qualquer ambiente.  Código Rust deve ser documentado no próprio código seguindo as melhores práticas de documentação.

Conclua a tarefa somente após garantir que todos os serviços deste repositório que são esperados pela interface estão concluídos. O código atual da interface está em ../../saci-interface/ .