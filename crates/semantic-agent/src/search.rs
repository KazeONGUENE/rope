//! Tantivy-backed semantic search over indexed knots.
//!
//! The schema is intentionally narrow — every field maps directly to a
//! [`crate::KnotIndexEntry`] field — so the index can be regenerated
//! deterministically from the agent's local view of the chain at any
//! point in time. That property is important for the merkle-rooted
//! checkpoint: if the agent's index ever diverges from the on-chain
//! truth, anyone can rebuild it from RPC and verify which side is
//! correct.

use crate::KnotIndexEntry;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, SchemaBuilder, FAST, INDEXED, STORED, STRING,
    TEXT,
};
use tantivy::{doc, Index, IndexReader, ReloadPolicy, TantivyDocument, Term};

/// Compile-time field bag for our schema. Held by [`SearchService`] so
/// queries can be built without re-resolving field names by string.
#[derive(Debug, Clone)]
struct Fields {
    knot_id: Field,
    string_id: Field,
    string_kind: Field,
    event_type: Field,
    knot_index: Field,
    status: Field,
    indexed_at: Field,
    knot_timestamp: Field,
    payload_text: Field,
    payload_size: Field,
}

fn build_schema() -> (Schema, Fields) {
    let mut sb: SchemaBuilder = Schema::builder();
    let knot_id = sb.add_text_field("knot_id", STRING | STORED);
    let string_id = sb.add_text_field("string_id", STRING | STORED);
    let string_kind = sb.add_text_field("string_kind", STRING | STORED);
    let event_type = sb.add_text_field("event_type", STRING | STORED);
    let status = sb.add_text_field("status", STRING | STORED);
    let payload_text = sb.add_text_field("payload_text", TEXT | STORED);
    let knot_index = sb.add_u64_field(
        "knot_index",
        NumericOptions::default()
            .set_indexed()
            .set_stored()
            .set_fast(),
    );
    let indexed_at = sb.add_i64_field(
        "indexed_at",
        NumericOptions::default()
            .set_indexed()
            .set_stored()
            .set_fast(),
    );
    let knot_timestamp = sb.add_i64_field(
        "knot_timestamp",
        NumericOptions::default()
            .set_indexed()
            .set_stored()
            .set_fast(),
    );
    let payload_size = sb.add_u64_field("payload_size", STORED | INDEXED | FAST);
    let schema = sb.build();
    (
        schema,
        Fields {
            knot_id,
            string_id,
            string_kind,
            event_type,
            knot_index,
            status,
            indexed_at,
            knot_timestamp,
            payload_text,
            payload_size,
        },
    )
}

/// One search result row. `score` is tantivy's BM25 ranking when a
/// full-text query is involved, otherwise tantivy's default constant
/// score for filter-only queries.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub entry: KnotIndexEntry,
    pub score: f32,
}

/// Search filters. All fields optional — an empty `SearchQuery` returns
/// the most recently indexed knots up to the configured limit.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SearchQuery {
    /// Free-text query against `payload_text`.
    pub q: Option<String>,
    pub event_type: Option<String>,
    pub string_kind: Option<String>,
    /// Owning string ID (creator wallet / contract / asset / did / cord).
    pub string_id: Option<String>,
    pub status: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: Option<u32>,
}

impl SearchQuery {
    pub fn limit_or_default(&self) -> usize {
        self.limit.unwrap_or(50).clamp(1, 1_000) as usize
    }
}

/// Tantivy-backed knot index. Open one per agent process — internally
/// thread-safe.
pub struct SearchService {
    index_path: PathBuf,
    index: Index,
    fields: Fields,
    /// `RwLock` rather than `Mutex` — readers are common (every
    /// `/v1/search` hit), writers (the indexer batch) are rare.
    writer: RwLock<Option<Arc<parking_lot::Mutex<tantivy::IndexWriter>>>>,
    reader: IndexReader,
}

impl SearchService {
    /// Open an existing index, or create a fresh empty one if the
    /// directory doesn't yet hold a tantivy schema.
    pub fn open_or_create<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let index_path = path.as_ref().to_path_buf();
        std::fs::create_dir_all(&index_path)?;
        let (schema, fields) = build_schema();

        let index = match Index::open_in_dir(&index_path) {
            Ok(i) => {
                // Sanity check: existing index must have the same schema.
                if i.schema() != schema {
                    return Err(anyhow::anyhow!(
                        "tantivy index at {:?} has incompatible schema (rebuild with a fresh dir)",
                        index_path
                    ));
                }
                i
            }
            Err(_) => Index::create_in_dir(&index_path, schema.clone())?,
        };

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        Ok(Self {
            index_path,
            index,
            fields,
            writer: RwLock::new(None),
            reader,
        })
    }

    fn ensure_writer(&self) -> anyhow::Result<Arc<parking_lot::Mutex<tantivy::IndexWriter>>> {
        if let Some(w) = self.writer.read().as_ref().cloned() {
            return Ok(w);
        }
        let mut slot = self.writer.write();
        if let Some(w) = slot.as_ref().cloned() {
            return Ok(w);
        }
        // 50 MB heap per writer — far more than needed for our schema
        // but cheap on a long-running daemon.
        let writer: tantivy::IndexWriter = self.index.writer(50_000_000)?;
        let arc = Arc::new(parking_lot::Mutex::new(writer));
        *slot = Some(arc.clone());
        Ok(arc)
    }

    /// Best-effort upsert: deletes any prior document with the same
    /// `knot_id`, then writes the new one. Caller batches via
    /// [`Self::index_entries`].
    pub fn index_entries(&self, entries: &[KnotIndexEntry]) -> anyhow::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let writer = self.ensure_writer()?;
        let mut w = writer.lock();
        for e in entries {
            // Idempotency: same knot_id replaces the previous record.
            let term = Term::from_field_text(self.fields.knot_id, &e.knot_id);
            w.delete_term(term);
            w.add_document(doc!(
                self.fields.knot_id => e.knot_id.clone(),
                self.fields.string_id => e.string_id.clone(),
                self.fields.string_kind => e.string_kind.clone(),
                self.fields.event_type => e.event_type.clone(),
                self.fields.status => e.status.clone(),
                self.fields.payload_text => e.payload_text.clone(),
                self.fields.knot_index => e.knot_index,
                self.fields.indexed_at => e.indexed_at,
                self.fields.knot_timestamp => e.knot_timestamp,
                self.fields.payload_size => e.payload_size,
            ))?;
        }
        w.commit()?;
        drop(w);
        self.reader.reload()?;
        Ok(())
    }

    /// Total documents in the index. Used by the checkpoint builder
    /// and the metrics endpoint.
    pub fn doc_count(&self) -> u64 {
        let searcher = self.reader.searcher();
        searcher.num_docs()
    }

    /// Run a [`SearchQuery`]. Always orders by `indexed_at` descending
    /// when no full-text query is supplied — i.e. "newest knots first"
    /// — so the API stays useful for live dashboards even when the
    /// caller hasn't typed a query yet.
    pub fn search(&self, q: &SearchQuery) -> anyhow::Result<Vec<SearchHit>> {
        let searcher = self.reader.searcher();
        let limit = q.limit_or_default();

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        if let Some(text) = q.q.as_ref().filter(|s| !s.trim().is_empty()) {
            let parser = QueryParser::for_index(&self.index, vec![self.fields.payload_text]);
            let parsed = parser.parse_query(text)?;
            clauses.push((Occur::Must, parsed));
        }
        if let Some(et) = q.event_type.as_ref().filter(|s| !s.is_empty()) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.event_type, et),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(k) = q.string_kind.as_ref().filter(|s| !s.is_empty()) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.string_kind, k),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(sid) = q.string_id.as_ref().filter(|s| !s.is_empty()) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.string_id, sid),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(st) = q.status.as_ref().filter(|s| !s.is_empty()) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.status, st),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if q.from.is_some() || q.to.is_some() {
            let lo = q.from.unwrap_or(i64::MIN);
            let hi = q.to.unwrap_or(i64::MAX);
            let range = RangeQuery::new_i64_bounds(
                "knot_timestamp".to_string(),
                std::ops::Bound::Included(lo),
                std::ops::Bound::Included(hi),
            );
            clauses.push((Occur::Must, Box::new(range)));
        }

        let query: Box<dyn Query> = if clauses.is_empty() {
            Box::new(AllQuery)
        } else {
            Box::new(BooleanQuery::new(clauses))
        };

        let collector = TopDocs::with_limit(limit)
            .order_by_fast_field::<i64>("indexed_at", tantivy::Order::Desc);
        // When the query has a real text component, BM25 scoring is
        // more useful than a pure timestamp ordering; we fall back to
        // it then.
        let hits = if q.q.as_ref().is_some_and(|t| !t.trim().is_empty()) {
            let top: Vec<(f32, tantivy::DocAddress)> =
                searcher.search(&*query, &TopDocs::with_limit(limit))?;
            top.into_iter()
                .map(|(score, addr)| {
                    let doc: TantivyDocument = searcher.doc(addr)?;
                    Ok::<_, anyhow::Error>(SearchHit {
                        entry: doc_to_entry(&doc, &self.fields),
                        score,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        } else {
            let top: Vec<(i64, tantivy::DocAddress)> = searcher.search(&*query, &collector)?;
            top.into_iter()
                .map(|(_ts, addr)| {
                    let doc: TantivyDocument = searcher.doc(addr)?;
                    Ok::<_, anyhow::Error>(SearchHit {
                        entry: doc_to_entry(&doc, &self.fields),
                        score: 1.0,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        };
        Ok(hits)
    }

    /// Snapshot of every indexed knot's identity tuple, sorted by
    /// `knot_id` ascending. This is the input to the checkpoint
    /// merkle-root computation. Performance: O(N) over the full
    /// index — acceptable at our checkpoint cadence (10 min) and at
    /// the indexed knot counts the agent will see in Phase 1
    /// (≤ 10⁷). For Phase 4 + 1.6 M anchors/s, this becomes a
    /// streaming merkle accumulator; out of scope for this crate.
    pub fn snapshot_identity_tuples(&self) -> anyhow::Result<Vec<(String, [u8; 32])>> {
        let searcher = self.reader.searcher();
        let mut all: Vec<(String, [u8; 32])> = Vec::new();
        for segment_reader in searcher.segment_readers() {
            let store_reader = segment_reader.get_store_reader(0)?;
            let alive = segment_reader.alive_bitset();
            for doc_id in 0..segment_reader.max_doc() {
                if let Some(b) = alive {
                    if !b.is_alive(doc_id) {
                        continue;
                    }
                }
                let doc: TantivyDocument = store_reader.get(doc_id)?;
                let entry = doc_to_entry(&doc, &self.fields);
                let digest = entry.identity_digest();
                all.push((entry.knot_id, digest));
            }
        }
        all.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(all)
    }

    /// Path the index lives at — exposed for diagnostics endpoints.
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }
}

fn doc_to_entry(doc: &TantivyDocument, fields: &Fields) -> KnotIndexEntry {
    use tantivy::schema::Value;
    let s = |f: Field| {
        doc.get_first(f)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    let u = |f: Field| doc.get_first(f).and_then(|v| v.as_u64()).unwrap_or(0);
    let i = |f: Field| doc.get_first(f).and_then(|v| v.as_i64()).unwrap_or(0);
    KnotIndexEntry {
        knot_id: s(fields.knot_id),
        string_id: s(fields.string_id),
        string_kind: s(fields.string_kind),
        event_type: s(fields.event_type),
        knot_index: u(fields.knot_index),
        status: s(fields.status),
        indexed_at: i(fields.indexed_at),
        knot_timestamp: i(fields.knot_timestamp),
        payload_text: s(fields.payload_text),
        payload_size: u(fields.payload_size),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: &str, etype: &str, ts: i64, body: &str) -> KnotIndexEntry {
        KnotIndexEntry {
            knot_id: id.to_string(),
            string_id: format!("0xowner{kind}"),
            string_kind: kind.to_string(),
            event_type: etype.to_string(),
            knot_index: 1,
            status: "active".to_string(),
            indexed_at: ts,
            knot_timestamp: ts,
            payload_text: body.to_string(),
            payload_size: body.len() as u64,
        }
    }

    fn fresh_service() -> SearchService {
        let dir = tempfile::tempdir().unwrap();
        // Leak the tempdir intentionally — the index outlives the test
        // function variable lifetime when held by the SearchService.
        let path = dir.keep();
        SearchService::open_or_create(path).unwrap()
    }

    #[test]
    fn index_and_search_by_event_type() {
        let svc = fresh_service();
        svc.index_entries(&[
            entry("0x01", "wallet", "Transfer", 100, "FAT transfer"),
            entry("0x02", "wallet", "TestimonySubmission", 200, "ai testimony"),
            entry("0x03", "asset", "Mint", 300, "DCNFT mint"),
        ])
        .unwrap();
        let hits = svc
            .search(&SearchQuery {
                event_type: Some("TestimonySubmission".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.knot_id, "0x02");
    }

    #[test]
    fn index_and_search_full_text() {
        let svc = fresh_service();
        svc.index_entries(&[
            entry("0x01", "wallet", "Transfer", 100, "transfer of tokens"),
            entry("0x02", "wallet", "TokenApproval", 200, "approve spend"),
            entry("0x03", "wallet", "TokenApproval", 200, "transfer approval"),
        ])
        .unwrap();
        let hits = svc
            .search(&SearchQuery {
                q: Some("transfer".into()),
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<_> = hits.iter().map(|h| h.entry.knot_id.clone()).collect();
        assert!(ids.contains(&"0x01".to_string()));
        assert!(ids.contains(&"0x03".to_string()));
        assert!(!ids.contains(&"0x02".to_string()));
    }

    #[test]
    fn index_and_search_time_range() {
        let svc = fresh_service();
        svc.index_entries(&[
            entry("0x01", "wallet", "Transfer", 100, ""),
            entry("0x02", "wallet", "Transfer", 200, ""),
            entry("0x03", "wallet", "Transfer", 300, ""),
        ])
        .unwrap();
        let hits = svc
            .search(&SearchQuery {
                from: Some(150),
                to: Some(250),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.knot_id, "0x02");
    }

    #[test]
    fn search_by_creator_string_id() {
        let svc = fresh_service();
        let mut e = entry("0x01", "wallet", "Transfer", 100, "");
        e.string_id = "0xowner-wallet-A".into();
        let mut e2 = entry("0x02", "wallet", "Transfer", 100, "");
        e2.string_id = "0xowner-wallet-B".into();
        svc.index_entries(&[e, e2]).unwrap();
        let hits = svc
            .search(&SearchQuery {
                string_id: Some("0xowner-wallet-A".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.knot_id, "0x01");
    }

    #[test]
    fn upsert_replaces_prior_document() {
        let svc = fresh_service();
        svc.index_entries(&[entry("0xdup", "wallet", "Transfer", 100, "v1")])
            .unwrap();
        svc.index_entries(&[entry("0xdup", "wallet", "Transfer", 200, "v2")])
            .unwrap();
        assert_eq!(svc.doc_count(), 1);
        let hits = svc.search(&SearchQuery::default()).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.payload_text, "v2");
    }

    #[test]
    fn snapshot_identity_tuples_is_sorted() {
        let svc = fresh_service();
        svc.index_entries(&[
            entry("0xcc", "wallet", "Transfer", 100, ""),
            entry("0xaa", "wallet", "Transfer", 200, ""),
            entry("0xbb", "wallet", "Transfer", 300, ""),
        ])
        .unwrap();
        let snap = svc.snapshot_identity_tuples().unwrap();
        let ids: Vec<_> = snap.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(ids, vec!["0xaa", "0xbb", "0xcc"]);
    }
}
