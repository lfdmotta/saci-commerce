//! Porta de armazenamento de documentos.
//!
//! Todo serviço guarda estado como **documentos JSON** endereçados por
//! partição (`pk`) e ordenação (`sk`), o desenho de tabela única do DynamoDB.
//! A porta só tem três operações, o mínimo que qualquer banco de chave e valor
//! com transação oferece:
//!
//! - [`DocumentStore::get`]: um documento;
//! - [`DocumentStore::query`]: uma partição, por prefixo de `sk`, paginada;
//! - [`DocumentStore::write`]: várias escritas **atômicas**, cada uma com
//!   condição ([`Expect`]), e eventos de *outbox* ([`WriteOp::Publish`]) na
//!   mesma transação.
//!
//! Índices secundários são documentos escritos na mesma transação (ex.:
//! `STF#<vitrine>` / `LST#<anúncio>`), o que dispensa GSI e funciona igual em
//! qualquer adaptador. Concorrência otimista por [`Item::version`] (o `ETag`
//! das rotas de edição).
//!
//! Adaptadores: [`MemoryStore`] (testes e servidor de desenvolvimento) e,
//! com a feature `aws`, `aws::dynamodb::DynamoStore`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use saci_events::CloudEvent;
use saci_problem::Problem;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::BoxFut;
use crate::problem::{internal, problem, unavailable};

/// Um documento guardado.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Partição.
    pub pk: String,
    /// Ordenação dentro da partição.
    pub sk: String,
    /// O documento.
    pub doc: Value,
    /// Versão (concorrência otimista). Começa em 1.
    pub version: u64,
    /// Expiração em segundos desde a época (TTL). `None` não expira.
    pub expires_at: Option<u64>,
}

impl Item {
    /// Cria um item na versão 1.
    pub fn new(
        pk: impl Into<String>,
        sk: impl Into<String>,
        doc: &impl Serialize,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            pk: pk.into(),
            sk: sk.into(),
            doc: serde_json::to_value(doc).map_err(|e| StoreError::Serialization(e.to_string()))?,
            version: 1,
            expires_at: None,
        })
    }

    /// Define a versão.
    #[must_use]
    pub fn with_version(mut self, version: u64) -> Self {
        self.version = version;
        self
    }

    /// Define a expiração.
    #[must_use]
    pub fn expiring(mut self, at: u64) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Lê o documento tipado.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, StoreError> {
        serde_json::from_value(self.doc.clone())
            .map_err(|e| StoreError::Serialization(format!("{}#{}: {e}", self.pk, self.sk)))
    }
}

/// Condição de uma escrita.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Sem condição.
    Any,
    /// O item não existe (criação única).
    Absent,
    /// O item existe.
    Present,
    /// O item existe nesta versão (edição sem atropelar outra pessoa).
    Version(u64),
}

/// Uma escrita numa transação.
#[derive(Debug, Clone)]
pub enum WriteOp {
    /// Grava o item inteiro.
    Put {
        /// O item, já com a versão nova.
        item: Item,
        /// Condição sobre o item atual.
        expect: Expect,
    },
    /// Apaga um item.
    Delete {
        /// Partição.
        pk: String,
        /// Ordenação.
        sk: String,
        /// Condição sobre o item atual.
        expect: Expect,
    },
    /// Só confere uma condição (sem escrever).
    Check {
        /// Partição.
        pk: String,
        /// Ordenação.
        sk: String,
        /// Condição.
        expect: Expect,
    },
    /// Evento de *outbox*: gravado na mesma transação e retransmitido ao
    /// barramento depois (seção 3.2).
    Publish(CloudEvent<Value>),
}

impl WriteOp {
    /// `Put` sem condição.
    #[must_use]
    pub fn put(item: Item) -> Self {
        Self::Put {
            item,
            expect: Expect::Any,
        }
    }

    /// `Put` que só cria.
    #[must_use]
    pub fn create(item: Item) -> Self {
        Self::Put {
            item,
            expect: Expect::Absent,
        }
    }

    /// `Put` que substitui a versão `previous`, gravando `previous + 1`.
    #[must_use]
    pub fn replace(item: Item, previous: u64) -> Self {
        Self::Put {
            item: item.with_version(previous + 1),
            expect: Expect::Version(previous),
        }
    }

    /// `Delete` sem condição.
    #[must_use]
    pub fn delete(pk: impl Into<String>, sk: impl Into<String>) -> Self {
        Self::Delete {
            pk: pk.into(),
            sk: sk.into(),
            expect: Expect::Any,
        }
    }
}

/// Consulta a uma partição.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Partição.
    pub pk: String,
    /// Prefixo de `sk` (vazio: a partição inteira).
    pub sk_prefix: String,
    /// Máximo de itens (0 = sem limite, o adaptador pagina por dentro).
    pub limit: usize,
    /// Continua depois desta `sk`.
    pub after: Option<String>,
    /// Ordem decrescente de `sk`.
    pub descending: bool,
}

impl Query {
    /// Partição inteira com prefixo.
    #[must_use]
    pub fn prefix(pk: impl Into<String>, sk_prefix: impl Into<String>) -> Self {
        Self {
            pk: pk.into(),
            sk_prefix: sk_prefix.into(),
            ..Self::default()
        }
    }

    /// Limita a página.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Continua depois de uma `sk`.
    #[must_use]
    pub fn after(mut self, sk: Option<String>) -> Self {
        self.after = sk;
        self
    }

    /// Ordem decrescente.
    #[must_use]
    pub fn desc(mut self) -> Self {
        self.descending = true;
        self
    }
}

/// Uma página de consulta.
#[derive(Debug, Clone, Default)]
pub struct Page {
    /// Os itens.
    pub items: Vec<Item>,
    /// A `sk` do último item, quando há mais.
    pub last: Option<String>,
}

/// Falha do armazenamento.
#[derive(Debug, Clone, thiserror::Error)]
pub enum StoreError {
    /// Uma condição da transação não foi atendida.
    #[error("condição da escrita não atendida")]
    Conflict,
    /// Falha do banco (rede, limite, permissão).
    #[error("falha do armazenamento: {0}")]
    Backend(String),
    /// Documento fora do formato esperado.
    #[error("documento inválido: {0}")]
    Serialization(String),
}

impl From<StoreError> for Problem {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Conflict => problem("invalid-state"),
            StoreError::Backend(_) => unavailable(&e),
            StoreError::Serialization(_) => internal(&e),
        }
    }
}

/// A porta.
pub trait DocumentStore: Send + Sync + fmt::Debug {
    /// Um item (expirado conta como ausente).
    fn get<'a>(&'a self, pk: &'a str, sk: &'a str) -> BoxFut<'a, Result<Option<Item>, StoreError>>;

    /// Uma página de uma partição.
    fn query(&self, query: Query) -> BoxFut<'_, Result<Page, StoreError>>;

    /// Escritas atômicas: todas ou nenhuma. Condição falha é [`StoreError::Conflict`].
    fn write(&self, ops: Vec<WriteOp>) -> BoxFut<'_, Result<(), StoreError>>;
}

/// Atalhos sobre a porta.
pub trait StoreExt: DocumentStore {
    /// Documento tipado e sua versão.
    fn load<'a, T: DeserializeOwned + 'a>(
        &'a self,
        pk: &'a str,
        sk: &'a str,
    ) -> BoxFut<'a, Result<Option<(T, u64)>, StoreError>> {
        Box::pin(async move {
            match self.get(pk, sk).await? {
                Some(item) => Ok(Some((item.parse()?, item.version))),
                None => Ok(None),
            }
        })
    }

    /// A partição inteira (com prefixo), paginando por dentro.
    fn query_all(
        &self,
        pk: String,
        sk_prefix: String,
    ) -> BoxFut<'_, Result<Vec<Item>, StoreError>> {
        Box::pin(async move {
            let mut out = Vec::new();
            let mut after = None;
            loop {
                let page = self
                    .query(Query {
                        pk: pk.clone(),
                        sk_prefix: sk_prefix.clone(),
                        limit: 0,
                        after,
                        descending: false,
                    })
                    .await?;
                out.extend(page.items);
                match page.last {
                    Some(last) => after = Some(last),
                    None => return Ok(out),
                }
            }
        })
    }
}

impl<T: DocumentStore + ?Sized> StoreExt for T {}

/// Recebe os eventos de *outbox* gravados pelo [`MemoryStore`].
pub trait EventSink: Send + Sync + fmt::Debug {
    /// Chamado depois que a transação gravou.
    fn publish(&self, events: Vec<CloudEvent<Value>>);
}

type Key = (String, String);

/// Adaptador em memória: testes e servidor de desenvolvimento.
///
/// Transações são atômicas (um só *lock* de escrita). Eventos de *outbox* vão
/// para o [`EventSink`] configurado, depois de gravados.
#[derive(Default)]
pub struct MemoryStore {
    data: RwLock<BTreeMap<Key, Item>>,
    sink: RwLock<Option<Arc<dyn EventSink>>>,
    clock: Option<Arc<dyn crate::clock::Clock>>,
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = self.data.read().map(|d| d.len()).unwrap_or_default();
        write!(f, "MemoryStore({len} itens)")
    }
}

impl MemoryStore {
    /// Armazenamento vazio.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Usa um relógio para a expiração (padrão: sistema).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn crate::clock::Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Liga o destino dos eventos.
    pub fn set_sink(&self, sink: Arc<dyn EventSink>) {
        if let Ok(mut s) = self.sink.write() {
            *s = Some(sink);
        }
    }

    /// Quantos itens (testes).
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.read().map(|d| d.len()).unwrap_or_default()
    }

    /// Vazio?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Todos os itens (exportação do servidor de desenvolvimento).
    #[must_use]
    pub fn dump(&self) -> Vec<Item> {
        self.data
            .read()
            .map(|d| d.values().cloned().collect())
            .unwrap_or_default()
    }

    fn now(&self) -> u64 {
        self.clock.as_ref().map_or_else(
            || crate::clock::Clock::epoch(&crate::clock::SystemClock),
            |c| c.epoch(),
        )
    }

    #[allow(clippy::unused_self)]
    fn live<'a>(&self, item: Option<&'a Item>, now: u64) -> Option<&'a Item> {
        item.filter(|i| i.expires_at.is_none_or(|t| t > now))
    }

    fn check(current: Option<&Item>, expect: Expect) -> bool {
        match (expect, current) {
            (Expect::Any, _) | (Expect::Present, Some(_)) | (Expect::Absent, None) => true,
            (Expect::Version(v), Some(i)) => i.version == v,
            _ => false,
        }
    }
}

impl DocumentStore for MemoryStore {
    fn get<'a>(&'a self, pk: &'a str, sk: &'a str) -> BoxFut<'a, Result<Option<Item>, StoreError>> {
        Box::pin(async move {
            let now = self.now();
            let data = self
                .data
                .read()
                .map_err(|_| StoreError::Backend("lock".into()))?;
            Ok(self
                .live(data.get(&(pk.to_owned(), sk.to_owned())), now)
                .cloned())
        })
    }

    fn query(&self, query: Query) -> BoxFut<'_, Result<Page, StoreError>> {
        Box::pin(async move {
            let now = self.now();
            let data = self
                .data
                .read()
                .map_err(|_| StoreError::Backend("lock".into()))?;
            let start = (query.pk.clone(), query.sk_prefix.clone());
            let mut items: Vec<&Item> = data
                .range(start..)
                .take_while(|((pk, sk), _)| *pk == query.pk && sk.starts_with(&query.sk_prefix))
                .map(|(_, v)| v)
                .filter(|i| self.live(Some(i), now).is_some())
                .collect();
            if query.descending {
                items.reverse();
            }
            if let Some(after) = &query.after {
                items.retain(|i| {
                    if query.descending {
                        i.sk < *after
                    } else {
                        i.sk > *after
                    }
                });
            }
            let limit = if query.limit == 0 {
                usize::MAX
            } else {
                query.limit
            };
            let more = items.len() > limit;
            items.truncate(limit);
            let last = if more {
                items.last().map(|i| i.sk.clone())
            } else {
                None
            };
            Ok(Page {
                items: items.into_iter().cloned().collect(),
                last,
            })
        })
    }

    fn write(&self, ops: Vec<WriteOp>) -> BoxFut<'_, Result<(), StoreError>> {
        Box::pin(async move {
            let now = self.now();
            let mut events = Vec::new();
            {
                let mut data = self
                    .data
                    .write()
                    .map_err(|_| StoreError::Backend("lock".into()))?;
                for op in &ops {
                    let (key, expect) = match op {
                        WriteOp::Put { item, expect } => {
                            ((item.pk.clone(), item.sk.clone()), *expect)
                        }
                        WriteOp::Delete { pk, sk, expect } | WriteOp::Check { pk, sk, expect } => {
                            ((pk.clone(), sk.clone()), *expect)
                        }
                        WriteOp::Publish(_) => continue,
                    };
                    if !Self::check(self.live(data.get(&key), now), expect) {
                        return Err(StoreError::Conflict);
                    }
                }
                for op in ops {
                    match op {
                        WriteOp::Put { item, .. } => {
                            data.insert((item.pk.clone(), item.sk.clone()), item);
                        }
                        WriteOp::Delete { pk, sk, .. } => {
                            data.remove(&(pk, sk));
                        }
                        WriteOp::Check { .. } => {}
                        WriteOp::Publish(event) => events.push(event),
                    }
                }
            }
            if !events.is_empty() {
                let sink = self.sink.read().ok().and_then(|s| s.clone());
                if let Some(sink) = sink {
                    sink.publish(events);
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn conditional_transactions_are_atomic() -> Result<(), StoreError> {
        let store = MemoryStore::new();
        let a = Item::new("P", "A", &json!({ "n": 1 }))?;
        store.write(vec![WriteOp::create(a.clone())]).await?;
        // Criar de novo falha, e a transação inteira não grava nada.
        let b = Item::new("P", "B", &json!({}))?;
        let r = store
            .write(vec![WriteOp::create(b), WriteOp::create(a.clone())])
            .await;
        assert!(matches!(r, Err(StoreError::Conflict)));
        assert!(store.get("P", "B").await?.is_none());
        // Edição com versão.
        store.write(vec![WriteOp::replace(a.clone(), 1)]).await?;
        assert_eq!(store.get("P", "A").await?.map(|i| i.version), Some(2));
        assert!(store.write(vec![WriteOp::replace(a, 1)]).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn queries_by_prefix_with_pages() -> Result<(), StoreError> {
        let store = MemoryStore::new();
        let ops = (0..5)
            .map(|i| Item::new("P", format!("X#{i}"), &json!(i)).map(WriteOp::put))
            .collect::<Result<Vec<_>, _>>()?;
        store.write(ops).await?;
        store
            .write(vec![WriteOp::put(Item::new("P", "Y#0", &json!(0))?)])
            .await?;
        let page = store.query(Query::prefix("P", "X#").limit(2)).await?;
        assert_eq!(page.items.len(), 2);
        let next = store
            .query(Query::prefix("P", "X#").limit(2).after(page.last))
            .await?;
        assert_eq!(next.items[0].sk, "X#2");
        assert_eq!(store.query_all("P".into(), "X#".into()).await?.len(), 5);
        let desc = store
            .query(Query::prefix("P", "X#").limit(1).desc())
            .await?;
        assert_eq!(desc.items[0].sk, "X#4");
        Ok(())
    }

    #[tokio::test]
    async fn expired_items_are_absent() -> Result<(), StoreError> {
        let clock = Arc::new(crate::clock::FixedClock::at(1_000));
        let store = MemoryStore::new().with_clock(clock.clone());
        store
            .write(vec![WriteOp::put(
                Item::new("P", "T", &json!(1))?.expiring(1_010),
            )])
            .await?;
        assert!(store.get("P", "T").await?.is_some());
        clock.advance(20);
        assert!(store.get("P", "T").await?.is_none());
        Ok(())
    }
}
