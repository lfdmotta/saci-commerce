# ADR 0003: Um binário por domínio, uma função por papel

- **Situação:** aceita
- **Data:** 2026-11-15

## Contexto

A diretiva pede paralelizar em Lambdas separadas tudo o que for paralelizável. O workflow reutilizável de implantação da plataforma implanta **um binário** por "serviço" (papel OIDC, estado e ambiente do GitHub por serviço). E um domínio tem papéis de perfil muito diferente: leitura pública intensa, escrita, consumidores de eventos, retransmissão do *outbox*, tarefas agendadas.

## Decisão

- Cada domínio tem um binário *multi-call* (`saci-<domínio>`, crate `domains/<domínio>/lambda`). Cada **função** Lambda usa o mesmo ZIP com `SACI_FUNCTION` diferente (`api`, `search`, `internal`, `indexer`, `relay`, `migrate`...).
- Cada função tem **papel IAM próprio**, memória, tempo e concorrência próprios (`infra/aws/modules/domain`).
- Para a plataforma, o "serviço" é o domínio: `saci-<env>-deploy-<domínio>`, estado em `services/<domínio>/`, recursos com prefixo `saci-<env>-<domínio>`.

## Consequências

- Leitura e escrita escalam e falham separadas; um consumidor lento não segura a API.
- Um build por domínio (não por função) e um ZIP por implantação.
- O binário contém o código de todos os papéis do domínio; o que cada função pode fazer é limitado pelo IAM, não pelo binário.
- Como várias funções dividem `/v1/<domínio>`, a árvore de rotas do domínio é criada num só lugar (`infra/aws/modules/routes`), com a função de cada rota.
