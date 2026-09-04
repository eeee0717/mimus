use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
use redb::ReadableTable;
use redb::{Database, DatabaseError, StorageError, TableDefinition};
use sha2::{Digest, Sha256};

use super::{Glossary, ValidatedTranslation};
use crate::error::{IoReason, MimusError, Result};

const TRANSLATIONS: TableDefinition<&[u8], &str> =
    TableDefinition::new("validated_translations_v2");
const IDENTITIES: TableDefinition<&[u8], u8> = TableDefinition::new("translation_identities_v1");
const EXTRACTED_GLOSSARIES: TableDefinition<&[u8], &str> =
    TableDefinition::new("extracted_glossaries_v1");
const KEY_SCHEMA: &[u8] = b"mimus-translation-cache-key-v2";
const TERMS_KEY_SCHEMA: &[u8] = b"mimus-term-extraction-cache-key-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranslationCacheKey([u8; 32]);

impl TranslationCacheKey {
    pub(crate) fn new(
        source: &str,
        model: &str,
        target_language: &str,
        prompt_version: &str,
        glossary_fingerprint: &str,
    ) -> Self {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, KEY_SCHEMA);
        for field in [
            source,
            model,
            target_language,
            prompt_version,
            glossary_fingerprint,
        ] {
            hash_field(&mut hasher, field.as_bytes());
        }
        Self(hasher.finalize().into())
    }

    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TermExtractionCacheKey([u8; 32]);

impl TermExtractionCacheKey {
    pub(crate) fn new(
        document_text: &str,
        model: &str,
        target_language: &str,
        prompt_version: &str,
    ) -> Self {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, TERMS_KEY_SCHEMA);
        for field in [document_text, model, target_language, prompt_version] {
            hash_field(&mut hasher, field.as_bytes());
        }
        Self(hasher.finalize().into())
    }

    fn bytes(&self) -> &[u8] {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[derive(Clone)]
pub(crate) struct TranslationCache {
    database: Arc<Database>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CachedTranslation {
    Translated(ValidatedTranslation),
    Identity,
}

impl TranslationCache {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|_| cache_error(path, "create directory"))?;
        }
        let database = match Database::create(path) {
            Ok(database) => database,
            Err(error) if database_is_corrupt(&error) => {
                isolate_corrupt_database(path)?;
                Database::create(path).map_err(|_| cache_error(path, "recreate database"))?
            }
            Err(_) => return Err(cache_error(path, "open database")),
        };
        let cache = Self {
            database: Arc::new(database),
        };
        cache.initialize(path)?;
        Ok(cache)
    }

    pub(crate) fn get(&self, key: &TranslationCacheKey) -> Result<Option<CachedTranslation>> {
        let read = self
            .database
            .begin_read()
            .map_err(|_| cache_operation_error("start read transaction"))?;
        let identities = read
            .open_table(IDENTITIES)
            .map_err(|_| cache_operation_error("open translation identity table"))?;
        if identities
            .get(key.bytes())
            .map_err(|_| cache_operation_error("read translation identity"))?
            .is_some()
        {
            return Ok(Some(CachedTranslation::Identity));
        }
        let table = read
            .open_table(TRANSLATIONS)
            .map_err(|_| cache_operation_error("open translation table"))?;
        let value = table
            .get(key.bytes())
            .map_err(|_| cache_operation_error("read translation"))?
            .map(|value| {
                CachedTranslation::Translated(ValidatedTranslation::from_cache(
                    value.value().to_owned(),
                ))
            });
        Ok(value)
    }

    pub(crate) fn insert(
        &self,
        key: &TranslationCacheKey,
        value: &ValidatedTranslation,
    ) -> Result<()> {
        let write = self
            .database
            .begin_write()
            .map_err(|_| cache_operation_error("start write transaction"))?;
        {
            let mut table = write
                .open_table(TRANSLATIONS)
                .map_err(|_| cache_operation_error("open translation table"))?;
            table
                .insert(key.bytes(), value.as_str())
                .map_err(|_| cache_operation_error("write translation"))?;
        }
        write
            .commit()
            .map_err(|_| cache_operation_error("commit translation"))
    }

    pub(crate) fn insert_identity(&self, key: &TranslationCacheKey) -> Result<()> {
        let write = self
            .database
            .begin_write()
            .map_err(|_| cache_operation_error("start write transaction"))?;
        {
            let mut table = write
                .open_table(IDENTITIES)
                .map_err(|_| cache_operation_error("open translation identity table"))?;
            table
                .insert(key.bytes(), 1)
                .map_err(|_| cache_operation_error("write translation identity"))?;
        }
        write
            .commit()
            .map_err(|_| cache_operation_error("commit translation identity"))
    }

    pub(crate) fn get_terms(&self, key: &TermExtractionCacheKey) -> Result<Option<Glossary>> {
        let read = self
            .database
            .begin_read()
            .map_err(|_| cache_operation_error("start read transaction"))?;
        let table = read
            .open_table(EXTRACTED_GLOSSARIES)
            .map_err(|_| cache_operation_error("open extracted glossary table"))?;
        table
            .get(key.bytes())
            .map_err(|_| cache_operation_error("read extracted glossary"))?
            .map(|value| {
                Glossary::from_toml(value.value())
                    .map_err(|_| cache_operation_error("decode extracted glossary"))
            })
            .transpose()
    }

    pub(crate) fn insert_terms(
        &self,
        key: &TermExtractionCacheKey,
        glossary: &Glossary,
    ) -> Result<()> {
        let value = glossary.canonical_toml();
        let write = self
            .database
            .begin_write()
            .map_err(|_| cache_operation_error("start write transaction"))?;
        {
            let mut table = write
                .open_table(EXTRACTED_GLOSSARIES)
                .map_err(|_| cache_operation_error("open extracted glossary table"))?;
            table
                .insert(key.bytes(), value.as_str())
                .map_err(|_| cache_operation_error("write extracted glossary"))?;
        }
        write
            .commit()
            .map_err(|_| cache_operation_error("commit extracted glossary"))
    }

    fn initialize(&self, path: &Path) -> Result<()> {
        let write = self
            .database
            .begin_write()
            .map_err(|_| cache_error(path, "start initialization transaction"))?;
        {
            write
                .open_table(TRANSLATIONS)
                .map_err(|_| cache_error(path, "initialize translation table"))?;
            write
                .open_table(IDENTITIES)
                .map_err(|_| cache_error(path, "initialize translation identity table"))?;
            write
                .open_table(EXTRACTED_GLOSSARIES)
                .map_err(|_| cache_error(path, "initialize extracted glossary table"))?;
        }
        write
            .commit()
            .map_err(|_| cache_error(path, "commit initialization"))
    }
}

#[cfg(test)]
pub(crate) struct MigratedTermCacheEntry {
    pub value: String,
}

#[cfg(test)]
pub(crate) fn migrate_unique_terms_entry(
    target_path: &Path,
    expected_old_key: &TermExtractionCacheKey,
    new_key: &TermExtractionCacheKey,
) -> Result<MigratedTermCacheEntry> {
    let cache = TranslationCache::open(target_path)?;
    let read = cache
        .database
        .begin_read()
        .map_err(|_| cache_operation_error("start migration read transaction"))?;
    let table = read
        .open_table(EXTRACTED_GLOSSARIES)
        .map_err(|_| cache_operation_error("open migration glossary table"))?;
    let entries = table
        .iter()
        .map_err(|_| cache_operation_error("iterate migration glossary table"))?
        .map(|entry| {
            let (key, value) =
                entry.map_err(|_| cache_operation_error("read migration glossary entry"))?;
            Ok((key.value().to_vec(), value.value().to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    let [(stored_key, value)] = entries.as_slice() else {
        return Err(cache_operation_error(
            "migrate glossary: source table must contain exactly one entry",
        ));
    };
    if stored_key.as_slice() != expected_old_key.bytes() {
        return Err(cache_operation_error(
            "migrate glossary: production old key does not match the unique stored key",
        ));
    }
    let value = value.clone();
    drop(table);
    drop(read);

    let write = cache
        .database
        .begin_write()
        .map_err(|_| cache_operation_error("start migration write transaction"))?;
    {
        let mut table = write
            .open_table(EXTRACTED_GLOSSARIES)
            .map_err(|_| cache_operation_error("open migration glossary table for writing"))?;
        table
            .insert(new_key.bytes(), value.as_str())
            .map_err(|_| cache_operation_error("copy migration glossary bytes"))?;
    }
    write
        .commit()
        .map_err(|_| cache_operation_error("commit migrated glossary entry"))?;
    Ok(MigratedTermCacheEntry { value })
}

fn database_is_corrupt(error: &DatabaseError) -> bool {
    match error {
        DatabaseError::UpgradeRequired(_)
        | DatabaseError::Storage(StorageError::Corrupted(_))
        | DatabaseError::RepairAborted => true,
        DatabaseError::Storage(StorageError::Io(error)) => matches!(
            error.kind(),
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

fn isolate_corrupt_database(path: &Path) -> Result<PathBuf> {
    for index in 0_u32.. {
        let mut name = OsString::from(path.as_os_str());
        if index == 0 {
            name.push(".corrupt");
        } else {
            name.push(format!(".corrupt.{index}"));
        }
        let backup = PathBuf::from(name);
        if backup.exists() {
            continue;
        }
        std::fs::rename(path, &backup)
            .map_err(|_| cache_error(path, "isolate corrupt database"))?;
        return Ok(backup);
    }
    unreachable!("u32 cache recovery suffixes cannot be exhausted")
}

fn cache_error(path: &Path, operation: &str) -> MimusError {
    MimusError::io(
        IoReason::CacheAccess,
        format!("could not {operation} at {}", path.display()),
    )
}

fn cache_operation_error(operation: &str) -> MimusError {
    MimusError::io(
        IoReason::CacheAccess,
        format!("could not {operation} in translation cache"),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Barrier;
    use std::thread;

    use super::*;

    fn key(source: &str) -> TranslationCacheKey {
        TranslationCacheKey::new(source, "model", "zh-CN", "prompt-v1", "glossary")
    }

    fn terms_key(document_text: &str) -> TermExtractionCacheKey {
        TermExtractionCacheKey::new(document_text, "model", "zh-CN", "terms-v1")
    }

    #[test]
    fn key_changes_when_any_owned_field_changes() {
        let keys = [
            TranslationCacheKey::new("source", "model", "zh-CN", "prompt-v1", "glossary"),
            TranslationCacheKey::new("other", "model", "zh-CN", "prompt-v1", "glossary"),
            TranslationCacheKey::new("source", "other", "zh-CN", "prompt-v1", "glossary"),
            TranslationCacheKey::new("source", "model", "fr", "prompt-v1", "glossary"),
            TranslationCacheKey::new("source", "model", "zh-CN", "prompt-v2", "glossary"),
            TranslationCacheKey::new("source", "model", "zh-CN", "prompt-v1", "other"),
        ];
        assert_eq!(
            keys.iter().map(|key| key.0).collect::<BTreeSet<_>>().len(),
            keys.len()
        );
    }

    #[test]
    fn term_key_changes_when_any_owned_field_changes() {
        let keys = [
            TermExtractionCacheKey::new("document", "model", "zh-CN", "terms-v1"),
            TermExtractionCacheKey::new("other", "model", "zh-CN", "terms-v1"),
            TermExtractionCacheKey::new("document", "other", "zh-CN", "terms-v1"),
            TermExtractionCacheKey::new("document", "model", "fr", "terms-v1"),
            TermExtractionCacheKey::new("document", "model", "zh-CN", "terms-v2"),
        ];
        assert_eq!(
            keys.iter().map(|key| key.0).collect::<BTreeSet<_>>().len(),
            keys.len()
        );
    }

    #[test]
    fn committed_values_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("translations.redb");
        let cache = TranslationCache::open(&path).unwrap();
        let stored = ValidatedTranslation::from_cache("translated".to_owned());

        cache.insert(&key("source"), &stored).unwrap();

        assert_eq!(
            cache.get(&key("source")).unwrap(),
            Some(CachedTranslation::Translated(stored))
        );
        for changed in [
            TranslationCacheKey::new("other", "model", "zh-CN", "prompt-v1", "glossary"),
            TranslationCacheKey::new("source", "other", "zh-CN", "prompt-v1", "glossary"),
            TranslationCacheKey::new("source", "model", "fr", "prompt-v1", "glossary"),
            TranslationCacheKey::new("source", "model", "zh-CN", "prompt-v2", "glossary"),
            TranslationCacheKey::new("source", "model", "zh-CN", "prompt-v1", "other"),
        ] {
            assert_eq!(cache.get(&changed).unwrap(), None);
        }
    }

    #[test]
    fn identity_outcomes_round_trip_without_storing_source_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("translations.redb");
        let cache = TranslationCache::open(&path).unwrap();
        let source = "identity-source-canary";

        cache.insert_identity(&key(source)).unwrap();

        assert_eq!(
            cache.get(&key(source)).unwrap(),
            Some(CachedTranslation::Identity)
        );
        let bytes = std::fs::read(path).unwrap();
        assert!(
            !bytes
                .windows(source.len())
                .any(|part| part == source.as_bytes())
        );
    }

    #[test]
    fn committed_extracted_glossaries_round_trip_canonically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("translations.redb");
        let cache = TranslationCache::open(&path).unwrap();
        let glossary = Glossary::from_toml(
            "version = 1\n[[terms]]\nsource = 'model'\ntarget = '模型'\n[[terms]]\nsource = 'cache'\ntarget = '缓存'\n",
        )
        .unwrap();

        cache
            .insert_terms(&terms_key("document"), &glossary)
            .unwrap();

        assert_eq!(
            cache.get_terms(&terms_key("document")).unwrap(),
            Some(glossary)
        );
        for changed in [
            TermExtractionCacheKey::new("other", "model", "zh-CN", "terms-v1"),
            TermExtractionCacheKey::new("document", "other", "zh-CN", "terms-v1"),
            TermExtractionCacheKey::new("document", "model", "fr", "terms-v1"),
            TermExtractionCacheKey::new("document", "model", "zh-CN", "terms-v2"),
        ] {
            assert_eq!(cache.get_terms(&changed).unwrap(), None);
        }
    }

    #[test]
    fn concurrent_transactions_leave_all_values_readable() {
        let directory = tempfile::tempdir().unwrap();
        let cache = TranslationCache::open(&directory.path().join("translations.redb")).unwrap();
        let barrier = Arc::new(Barrier::new(9));
        let handles = (0..8)
            .map(|index| {
                let cache = cache.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    cache
                        .insert(
                            &key(&format!("source-{index}")),
                            &ValidatedTranslation::from_cache(format!("translated-{index}")),
                        )
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        for index in 0..8 {
            assert_eq!(
                cache.get(&key(&format!("source-{index}"))).unwrap(),
                Some(CachedTranslation::Translated(
                    ValidatedTranslation::from_cache(format!("translated-{index}"))
                ))
            );
        }
    }

    #[test]
    fn corrupt_database_is_isolated_and_recreated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("translations.redb");
        std::fs::write(&path, b"not a redb database").unwrap();

        let cache = TranslationCache::open(&path).unwrap();
        let stored = ValidatedTranslation::from_cache("translated".to_owned());
        cache.insert(&key("source"), &stored).unwrap();

        assert_eq!(
            cache.get(&key("source")).unwrap(),
            Some(CachedTranslation::Translated(stored))
        );
        assert_eq!(
            std::fs::read(directory.path().join("translations.redb.corrupt")).unwrap(),
            b"not a redb database"
        );
    }
}
