use dn_knowledge::{
    Citation, EmbedKind, Embedder, Index, QueryVector, SearchMode, SourceFile, Sources,
    chunk::Skip, context_block, delete_all, migrations, record_sources,
    remove_conversation_sources, sources_for,
};
use rusqlite::Connection;
use std::cell::Cell;

fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    for m in migrations() {
        conn.execute_batch(m).unwrap();
    }
    conn
}

fn file(id: &str, path: &str, text: &str) -> SourceFile {
    SourceFile {
        file_id: id.into(),
        display_path: path.into(),
        content: text.as_bytes().to_vec(),
    }
}

/// Deterministic "embedding": a normalised bag of word buckets, plus a synonym table so the
/// vector side can find what BM25 can't (no shared words).
struct FakeEmbedder {
    model: &'static str,
    dim: usize,
    calls: Cell<usize>,
    kinds: std::cell::RefCell<Vec<EmbedKind>>,
}

impl FakeEmbedder {
    fn new(model: &'static str) -> Self {
        Self::with_dim(model, 64)
    }
    fn with_dim(model: &'static str, dim: usize) -> Self {
        Self {
            model,
            dim,
            calls: Cell::new(0),
            kinds: Default::default(),
        }
    }
}

impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &str {
        self.model
    }
    fn dimension(&self) -> usize {
        self.dim
    }
    fn embed(&self, texts: &[String], kind: EmbedKind) -> Result<Vec<Vec<f32>>, String> {
        self.calls.set(self.calls.get() + 1);
        self.kinds.borrow_mut().push(kind);
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; self.dim];
                for w in t.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
                    let w = match w {
                        "automobile" | "vehicle" => "car",
                        other => other,
                    };
                    if w.is_empty() {
                        continue;
                    }
                    let h = w
                        .bytes()
                        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
                    v[h as usize % self.dim] += 1.0;
                }
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                v.iter().map(|x| x / n).collect()
            })
            .collect())
    }
}

#[test]
fn bm25_finds_the_chunk_and_cites_file_and_lines() {
    let conn = db();
    let idx = Index::new(&conn);
    let mut plan = String::new();
    for i in 1..=60 {
        plan.push_str(&format!("filler line {i}\n"));
    }
    plan.push_str("The launch date is the 14th of May.\n");
    let r = idx
        .sync_source(
            "p1",
            &[
                file("f1", "docs/plan.md", &plan),
                file("f2", "notes.txt", "Groceries: milk, eggs."),
            ],
            None,
        )
        .unwrap();
    assert_eq!((r.indexed, r.unchanged, r.removed), (2, 0, 0));
    let hits = idx
        .search(&["p1"], "when is the launch date?", 3, None)
        .unwrap();
    assert_eq!(hits[0].display_path, "docs/plan.md");
    assert!(hits[0].text.contains("14th of May"));
    assert!(
        hits[0].start_line <= 61 && 61 <= hits[0].end_line,
        "{hits:?}"
    );
    let (block, cites) = context_block(&hits[..1]);
    assert!(block.contains("file=\"docs/plan.md\""));
    assert_eq!(cites[0].display_path, "docs/plan.md");
}

#[test]
fn query_text_is_never_fts_syntax() {
    let conn = db();
    let idx = Index::new(&conn);
    idx.sync_source("p", &[file("f", "a.md", "alpha beta gamma")], None)
        .unwrap();
    // FTS operators and quotes in user text must not error or change the meaning.
    for q in [
        "alpha\" OR \"x",
        "NEAR(alpha beta)",
        "beta*",
        "a:b",
        "\"",
        "-alpha",
        "()",
    ] {
        idx.search(&["p"], q, 5, None).unwrap();
    }
    assert!(idx.search(&["p"], "   ", 5, None).unwrap().is_empty());
}

#[test]
fn embeddings_find_what_keywords_miss_and_fuse_with_bm25() {
    let conn = db();
    let idx = Index::new(&conn);
    let e = FakeEmbedder::new("fake-1");
    idx.sync_source(
        "p",
        &[
            file("f1", "garage.md", "My car needs new tyres before winter."),
            file("f2", "cook.md", "Bake the bread at 220 degrees."),
        ],
        Some(&e),
    )
    .unwrap();
    // No word in common with garage.md: BM25 alone finds nothing...
    assert!(
        idx.search(&["p"], "automobile", 5, None)
            .unwrap()
            .is_empty()
    );
    // ...the vector side does.
    let hits = idx.search(&["p"], "automobile", 5, Some(&e)).unwrap();
    assert_eq!(hits[0].display_path, "garage.md");
}

#[test]
fn sync_is_incremental_and_follows_renames_edits_and_removals() {
    let conn = db();
    let idx = Index::new(&conn);
    let e = FakeEmbedder::new("fake-1");
    idx.sync_source(
        "p",
        &[file("f1", "a.md", "one"), file("f2", "b.md", "two")],
        Some(&e),
    )
    .unwrap();
    let calls = e.calls.get();
    // Same content (renamed): kept, not re-embedded; the citation path follows the rename.
    let r = idx
        .sync_source(
            "p",
            &[
                file("f1", "renamed.md", "one"),
                file("f2", "b.md", "two changed"),
            ],
            Some(&e),
        )
        .unwrap();
    assert_eq!(
        (r.indexed, r.unchanged, r.removed, r.embedded),
        (1, 1, 0, 1)
    );
    assert_eq!(e.calls.get(), calls + 1);
    assert_eq!(
        idx.search(&["p"], "one", 5, None).unwrap()[0].display_path,
        "renamed.md"
    );
    assert!(
        idx.search(&["p"], "two", 5, None).unwrap()[0]
            .text
            .contains("changed")
    );
    // A file no longer listed is removed with its chunks.
    let r = idx
        .sync_source("p", &[file("f2", "b.md", "two changed")], None)
        .unwrap();
    assert_eq!(r.removed, 1);
    assert!(idx.search(&["p"], "one", 5, None).unwrap().is_empty());
    // A new embedding model re-embeds everything.
    let e2 = FakeEmbedder::new("fake-2");
    let r = idx
        .sync_source("p", &[file("f2", "b.md", "two changed")], Some(&e2))
        .unwrap();
    assert_eq!(r.embedded, 1);
}

#[test]
fn binary_large_and_empty_files_are_skipped_and_reported() {
    let conn = db();
    let idx = Index::new(&conn);
    let r = idx
        .sync_source(
            "p",
            &[
                SourceFile {
                    file_id: "bin".into(),
                    display_path: "img.png".into(),
                    content: vec![0x89, b'P', b'N', b'G', 0, 1, 2],
                },
                file("e", "empty.md", "  \n"),
                file("ok", "ok.md", "fine"),
            ],
            None,
        )
        .unwrap();
    assert_eq!(r.indexed, 1);
    assert_eq!(
        r.skipped,
        vec![
            ("img.png".into(), Skip::NotText),
            ("empty.md".into(), Skip::Empty)
        ]
    );
}

#[test]
fn searches_stay_inside_their_sources() {
    let conn = db();
    let idx = Index::new(&conn);
    idx.sync_source("p1", &[file("f", "a.md", "secret plan alpha")], None)
        .unwrap();
    idx.sync_source("p2", &[file("f", "b.md", "public plan beta")], None)
        .unwrap();
    let hits = idx.search(&["p2"], "plan alpha", 5, None).unwrap();
    assert!(hits.iter().all(|h| h.source_id == "p2"), "{hits:?}");
    assert!(idx.search(&[], "plan", 5, None).unwrap().is_empty());
    idx.remove_source("p1").unwrap();
    assert!(idx.search(&["p1"], "secret", 5, None).unwrap().is_empty());
    assert_eq!(idx.counts().unwrap().files, 1);
}

#[test]
fn delete_all_leaves_no_text_behind() {
    let conn = db();
    let idx = Index::new(&conn);
    idx.sync_source(
        "p",
        &[file("f", "a.md", "remember the passphrase zebra")],
        Some(&FakeEmbedder::new("m")),
    )
    .unwrap();
    delete_all(&conn).unwrap();
    assert_eq!(idx.counts().unwrap().chunks, 0);
    assert!(idx.search(&["p"], "zebra", 5, None).unwrap().is_empty());
    // Nothing left in FTS5's own shadow tables either.
    let leftover: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM knowledge_fts_data WHERE instr(block, CAST('zebra' AS BLOB)) > 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
}

fn fts_has(conn: &Connection, word: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM knowledge_fts_data WHERE instr(block, CAST(?1 AS BLOB)) > 0",
        [word],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn removing_a_source_leaves_none_of_its_text_in_the_fts_index() {
    let conn = db();
    let idx = Index::new(&conn);
    idx.sync_source("gone", &[file("f", "a.md", "the codeword is quokka")], None)
        .unwrap();
    idx.sync_source("kept", &[file("f", "b.md", "the codeword is wombat")], None)
        .unwrap();
    assert!(fts_has(&conn, "quokka") > 0);
    idx.remove_source("gone").unwrap();
    assert_eq!(fts_has(&conn, "quokka"), 0);
    assert!(fts_has(&conn, "wombat") > 0);
    assert_eq!(idx.search(&["kept"], "wombat", 5, None).unwrap().len(), 1);
}

#[test]
fn documents_and_queries_are_embedded_as_such() {
    let conn = db();
    let idx = Index::new(&conn);
    let e = FakeEmbedder::new("m");
    idx.sync_source("p", &[file("f", "a.md", "hello world")], Some(&e))
        .unwrap();
    assert_eq!(*e.kinds.borrow(), vec![EmbedKind::Document]);
    idx.search(&["p"], "hello", 3, Some(&e)).unwrap();
    assert_eq!(e.kinds.borrow().last(), Some(&EmbedKind::Query));
}

#[test]
fn vectors_of_another_size_are_never_mixed_but_re_embedded() {
    let conn = db();
    let idx = Index::new(&conn);
    let small = FakeEmbedder::with_dim("m", 32);
    idx.sync_source(
        "p",
        &[file("f", "garage.md", "my car needs tyres")],
        Some(&small),
    )
    .unwrap();
    // Same model id, other size: nothing to compare against yet (no mixed vectors)...
    let big = FakeEmbedder::with_dim("m", 64);
    assert!(
        idx.search(&["p"], "automobile", 3, Some(&big))
            .unwrap()
            .is_empty()
    );
    // ...until a sync re-embeds with it.
    let r = idx
        .sync_source(
            "p",
            &[file("f", "garage.md", "my car needs tyres")],
            Some(&big),
        )
        .unwrap();
    assert_eq!(r.embedded, 1);
    assert_eq!(
        idx.search(&["p"], "automobile", 3, Some(&big))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_model_returning_the_wrong_size_is_an_error() {
    struct Liar;
    impl Embedder for Liar {
        fn model_id(&self) -> &str {
            "liar"
        }
        fn dimension(&self) -> usize {
            8
        }
        fn embed(&self, texts: &[String], _: EmbedKind) -> Result<Vec<Vec<f32>>, String> {
            Ok(texts.iter().map(|_| vec![0.5; 4]).collect())
        }
    }
    let conn = db();
    let idx = Index::new(&conn);
    assert!(
        idx.sync_source("p", &[file("f", "a.md", "text")], Some(&Liar))
            .is_err()
    );
    // The text is still indexed for keyword search.
    assert_eq!(idx.search(&["p"], "text", 3, None).unwrap().len(), 1);
}

#[test]
fn sources_lists_what_is_indexed() {
    let conn = db();
    let idx = Index::new(&conn);
    idx.sync_source("b", &[file("f", "x.md", "x")], None)
        .unwrap();
    idx.sync_source("a", &[file("f", "y.md", "y")], None)
        .unwrap();
    assert_eq!(
        idx.sources().unwrap(),
        vec!["a".to_string(), "b".to_string()]
    );
    idx.remove_source("a").unwrap();
    assert_eq!(idx.sources().unwrap(), vec!["b".to_string()]);
}

#[test]
fn removing_or_reindexing_one_file_leaves_none_of_its_old_text_in_the_fts_index() {
    let conn = db();
    let idx = Index::new(&conn);
    idx.sync_source(
        "p",
        &[
            file("a", "a.md", "codeword capybara"),
            file("b", "b.md", "codeword ocelot"),
        ],
        None,
    )
    .unwrap();
    assert!(fts_has(&conn, "capybara") > 0 && fts_has(&conn, "ocelot") > 0);
    // One file removed from the project: its text goes, the other file's stays.
    idx.sync_source("p", &[file("b", "b.md", "codeword ocelot")], None)
        .unwrap();
    assert_eq!(fts_has(&conn, "capybara"), 0);
    assert!(fts_has(&conn, "ocelot") > 0);
    // A file re-indexed with new content: the old text goes too.
    idx.sync_source("p", &[file("b", "b.md", "codeword narwhal")], None)
        .unwrap();
    assert_eq!(fts_has(&conn, "ocelot"), 0);
    assert!(fts_has(&conn, "narwhal") > 0);
}

#[test]
fn the_caller_can_embed_outside_the_database_in_two_phases() {
    let conn = db();
    let idx = Index::new(&conn);
    let e = FakeEmbedder::new("m");
    idx.sync_source(
        "p",
        &[
            file("f1", "garage.md", "my car needs tyres"),
            file("f2", "cook.md", "bake bread"),
        ],
        None,
    )
    .unwrap();
    let todo = idx.pending_embeddings(None, "m", 64, 10).unwrap();
    assert_eq!(todo.len(), 2);
    let texts: Vec<String> = todo.iter().map(|(_, t)| t.clone()).collect();
    let vecs = e.embed(&texts, EmbedKind::Document).unwrap();
    let items: Vec<(i64, Vec<f32>)> = todo.iter().map(|(id, _)| *id).zip(vecs).collect();
    assert_eq!(idx.store_embeddings("m", 64, &items).unwrap(), 2);
    assert!(
        idx.pending_embeddings(None, "m", 64, 10)
            .unwrap()
            .is_empty()
    );
    // Wrong-size vectors are refused.
    assert!(
        idx.store_embeddings("m", 64, &[(items[0].0, vec![0.0; 3])])
            .is_err()
    );
    // A query vector made by the caller ranks like the embedder path.
    let qv = QueryVector {
        model_id: "m".into(),
        values: e
            .embed(&["automobile".into()], EmbedKind::Query)
            .unwrap()
            .remove(0),
    };
    let hits = idx.search_with(&["p"], "automobile", 3, Some(&qv)).unwrap();
    assert_eq!(hits[0].display_path, "garage.md");
}

#[test]
fn an_answers_sources_are_recorded_and_go_with_its_conversation_and_delete_all() {
    let conn = db();
    let cite = Citation {
        label: "S1".into(),
        chunk_id: 1,
        file_id: "f".into(),
        display_path: "plan.md".into(),
        start_line: 3,
        end_line: 9,
    };
    let s = Sources {
        mode: SearchMode::Keyword,
        citations: vec![cite.clone()],
    };
    record_sources(&conn, "m1", "c1", &s).unwrap();
    record_sources(
        &conn,
        "m2",
        "c2",
        &Sources {
            mode: SearchMode::Hybrid,
            citations: vec![],
        },
    )
    .unwrap();
    assert_eq!(sources_for(&conn, "m1").unwrap(), Some(s));
    assert_eq!(sources_for(&conn, "nope").unwrap(), None);
    // Regenerating replaces the record.
    record_sources(
        &conn,
        "m1",
        "c1",
        &Sources {
            mode: SearchMode::Hybrid,
            citations: vec![cite],
        },
    )
    .unwrap();
    assert_eq!(
        sources_for(&conn, "m1").unwrap().unwrap().mode,
        SearchMode::Hybrid
    );
    remove_conversation_sources(&conn, "c1").unwrap();
    assert_eq!(sources_for(&conn, "m1").unwrap(), None);
    delete_all(&conn).unwrap();
    assert_eq!(sources_for(&conn, "m2").unwrap(), None);
}
