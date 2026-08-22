//! Single-variable byte mutations derived from a known legal parent PDF.

use anyhow::{Context, Result, ensure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationSpec<'a> {
    pub parent_fixture_id: &'a str,
    pub byte_offset: usize,
    pub expected_byte: u8,
    pub replacement_byte: u8,
    pub semantics: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRecord {
    pub parent_fixture_id: String,
    pub byte_offset: u64,
    pub original_byte: u8,
    pub replacement_byte: u8,
    pub semantics: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedFixture {
    pub bytes: Vec<u8>,
    pub record: MutationRecord,
}

pub fn derive(parent: &[u8], spec: MutationSpec<'_>) -> Result<DerivedFixture> {
    ensure!(
        spec.parent_fixture_id.starts_with("unit-") || spec.parent_fixture_id.starts_with("intg-"),
        "parent fixture must be legal (unit-* or intg-*): {}",
        spec.parent_fixture_id
    );
    ensure!(
        !spec.semantics.trim().is_empty(),
        "mutation semantics must not be empty"
    );
    let original = *parent
        .get(spec.byte_offset)
        .with_context(|| format!("mutation offset {} is outside parent", spec.byte_offset))?;
    ensure!(
        original == spec.expected_byte,
        "parent byte drift at offset {}: expected 0x{:02x}, found 0x{original:02x}",
        spec.byte_offset,
        spec.expected_byte
    );
    ensure!(
        spec.replacement_byte != original,
        "replacement byte must differ at offset {}",
        spec.byte_offset
    );

    let mut bytes = parent.to_vec();
    bytes[spec.byte_offset] = spec.replacement_byte;
    Ok(DerivedFixture {
        bytes,
        record: MutationRecord {
            parent_fixture_id: spec.parent_fixture_id.to_string(),
            byte_offset: u64::try_from(spec.byte_offset).context("mutation offset exceeds u64")?,
            original_byte: original,
            replacement_byte: spec.replacement_byte,
            semantics: spec.semantics.to_string(),
        },
    })
}

/// Verify that a committed child differs from its parent at exactly the one
/// byte described by `record`.
pub fn verify(parent: &[u8], child: &[u8], record: &MutationRecord) -> Result<()> {
    ensure!(
        parent.len() == child.len(),
        "byte mutation changed file length: parent {}, child {}",
        parent.len(),
        child.len()
    );
    ensure!(
        !record.semantics.trim().is_empty(),
        "mutation semantics must not be empty"
    );
    let offset = usize::try_from(record.byte_offset).context("mutation offset exceeds usize")?;
    ensure!(
        parent.get(offset) == Some(&record.original_byte),
        "recorded original byte does not match parent at offset {offset}"
    );
    ensure!(
        child.get(offset) == Some(&record.replacement_byte),
        "recorded replacement byte does not match child at offset {offset}"
    );
    let differences: Vec<usize> = parent
        .iter()
        .zip(child)
        .enumerate()
        .filter_map(|(index, (before, after))| (before != after).then_some(index))
        .collect();
    ensure!(
        differences == [offset],
        "child has unrecorded byte changes; declared [{offset}], actual {differences:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_exactly_one_recorded_byte_from_the_legal_parent() {
        let parent = b"%PDF-1.7\nstream\n10 0 0 10\nendstream";
        let offset = parent.windows(2).position(|bytes| bytes == b"10").unwrap();
        let derived = derive(
            parent,
            MutationSpec {
                parent_fixture_id: "unit-base-01-single-line",
                byte_offset: offset,
                expected_byte: b'1',
                replacement_byte: b'Q',
                semantics: "glue the numeric operand to operator Q",
            },
        )
        .unwrap();

        let differences: Vec<usize> = parent
            .iter()
            .zip(&derived.bytes)
            .enumerate()
            .filter_map(|(index, (before, after))| (before != after).then_some(index))
            .collect();
        assert_eq!(differences, vec![offset]);
        assert_eq!(derived.bytes.len(), parent.len());
        assert_eq!(
            derived.record,
            MutationRecord {
                parent_fixture_id: "unit-base-01-single-line".into(),
                byte_offset: offset as u64,
                original_byte: b'1',
                replacement_byte: b'Q',
                semantics: "glue the numeric operand to operator Q".into(),
            }
        );
    }

    #[test]
    fn rejects_a_child_that_changes_an_unrecorded_second_byte() {
        let parent = b"abcdef";
        let child = b"aXcdeY";
        let record = MutationRecord {
            parent_fixture_id: "unit-base-01-single-line".into(),
            byte_offset: 1,
            original_byte: b'b',
            replacement_byte: b'X',
            semantics: "change the selected token".into(),
        };

        let error = verify(parent, child, &record).unwrap_err();
        assert!(error.to_string().contains("unrecorded"), "{error:#}");
    }
}
