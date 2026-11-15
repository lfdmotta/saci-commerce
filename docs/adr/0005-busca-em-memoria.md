# ADR 0005: Busca sobre projeção em DynamoDB, lida em memória

- **Situação:** aceita
- **Data:** 2026-11-15

## Contexto

A proposta põe a busca no OpenSearch (3.1). Para o lançamento (dezenas de vitrines, poucos milhares de peças), um domínio OpenSearch custa por hora, exige sub-rede isolada e reindexação, e a busca pedida é simples: palavras sem acento, filtros e facetas.

## Decisão

- O `search-indexer` mantém a **projeção pública** (anúncio pronto, bancas do diretório, categorias) numa tabela própria, por eventos do catálogo.
- A função `search` carrega a projeção inteira na memória e a renova a cada 30 s; cada consulta é um filtro em memória. As regras de busca, ordem e facetas são as dos mocks da interface.

## Consequências

- Custo por requisição, sem nada ligado por hora; respostas em microssegundos depois de carregada.
- Limite prático: a projeção precisa caber na memória da função (dezenas de milhares de peças em 512 MB a 1 GB). Passando disso, ou para busca semântica, troca-se só esta leitura por OpenSearch; o indexador e os contratos continuam.
- A busca pode atrasar até 30 s depois de uma mudança.
