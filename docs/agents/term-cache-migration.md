# Offline term-cache migration

An automatic term-extraction cache key includes the complete production `document_text`. A policy
change that removes prepared translation requests can therefore invalidate that key even when an
existing extracted glossary remains applicable. Never invent the replacement key or copy a cache
entry without an explicit adjudication.

The dev-only ignored test
`pass::tests::migrate_bert_term_cache_after_author_geometry_policy` implements the M3.7 BERT
adjudication. It runs the production Parse through StylesAndFormulas path with the pinned PDFium and
ONNX assets, reconstructs the old author policy in memory, and requires the only request difference
to be page 0 reading orders 11 and 12. Both old and new keys use the production
`TermExtractionCacheKey::new` and prompt-version constant.

Before running it, copy the immutable cache and make only the copy writable:

```sh
cp "$SOURCE_CACHE" "$RUN_DIR/05-bert-m3-7-author-geometry.redb"
chmod u+w "$RUN_DIR/05-bert-m3-7-author-geometry.redb"

MIMUS_PDFIUM_LIBRARY="$PDFIUM_LIBRARY" \
MIMUS_TERM_MIGRATION_SOURCE_CACHE="$SOURCE_CACHE" \
MIMUS_TERM_MIGRATION_TARGET_CACHE="$RUN_DIR/05-bert-m3-7-author-geometry.redb" \
MIMUS_TERM_MIGRATION_PDF="$BERT_PDF" \
MIMUS_TERM_MIGRATION_LAYOUT_MODEL="$LAYOUT_MODEL" \
MIMUS_TERM_MIGRATION_MODEL=m35-proxy-model \
MIMUS_TERM_MIGRATION_TARGET_LANGUAGE=zh-CN \
MIMUS_TERM_MIGRATION_DATE=2026-09-03 \
cargo test --locked --offline -p mimus-core \
  pass::tests::migrate_bert_term_cache_after_author_geometry_policy -- --ignored --exact
```

The tool refuses to run unless the source is read-only, source and target are distinct, the target
starts byte-identical to the source, the glossary table contains exactly one entry, the reconstructed
old key equals that entry, and the only removed requests are the adjudicated two paragraphs. It
copies the stored glossary string without parsing or reserializing it. The archive is read only for
hashing and is checked again after migration.

The sidecar is written beside the target as `05-bert-m3-7-author-geometry.provenance.json`. Schema 1
contains the source and resulting target cache hashes; old/new production keys; old/new
`document_text` hashes and byte lengths; page, reading order, and request hash for each removed
paragraph; glossary fingerprint; model, language, and prompt version; date; and `model_calls: 0`.
Keep the cache and sidecar together as one provenance unit. Paper bytes and paragraph text never
enter the sidecar or repository.

After migration, replay only against a confirmed-closed loopback endpoint with a fake key. Any
cache miss or transport attempt means the adjudicated equivalence was insufficient: stop, retain the
artifacts for diagnosis, and do not call a real provider or extend the migration.
