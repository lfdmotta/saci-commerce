//! `search-indexer`: a busca do catálogo (proposta 3.1, serviço `search`).
//!
//! Duas funções, cada uma na sua Lambda:
//!
//! - [`indexer::Indexer`]: consome os eventos do catálogo e mantém a
//!   projeção pública (peças, bancas, categorias) numa tabela própria;
//! - [`search::Search`]: atende as leituras filtradas da feira a partir da
//!   projeção carregada em memória.
//!
//! A troca para OpenSearch (busca semântica, catálogos grandes) muda só o
//! adaptador desta leitura; os contratos e o indexador continuam (ADR 0005).

pub mod indexer;
pub mod projection;
pub mod search;

pub use indexer::Indexer;
pub use search::Search;
