# content-publisher

Publica os layouts das telas (`domains/content/layouts/*.json`) em `cdn.saci.art.br/layouts` (proposta 5.14).

```
pull request aprovado
  → validação de esquema e de endereços (saci-contracts/tooling/validate-layout.mjs)
  → balde do CDN da plataforma (versionado)
  → invalidação do CloudFront em /layouts/*
```

| Regra | Como |
| --- | --- |
| Formato | `ScreenLayout` de `saci-contracts/layouts/screen-layout.schema.yaml`, sem nenhuma mudança de formato na interface |
| Endereços (S-17) | `href` só rota interna ou `saci.art.br`; imagens só nos hosts de mídia (`layouts/allowed-hosts.yaml`) |
| Imagens | Ficam em `layouts/media/` neste repositório e são publicadas com o JSON; no arquivo, sempre o host de produção (`cdn.saci.art.br`), trocado pelo do ambiente na publicação |
| Quem escreve | Só o papel OIDC do content-publisher (`static_publishers` em `ci/aws/stacks/github-oidc` do saci-boiuna, prefixo `layouts/`) |

## Comandos

```bash
task layouts:validate              # só valida (precisa do saci-contracts ao lado)
task layouts:publish ENV=dev       # valida e publica (credenciais do ambiente)
```

No servidor de desenvolvimento, os layouts saem de `http://localhost:8787/layouts/<tela>.json`, já com as imagens apontando para o próprio servidor.

O workflow `.github/workflows/layouts.yml` publica em dev a cada mudança na `main` e em produção com aprovação.
