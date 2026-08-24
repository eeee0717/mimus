//! Single-variable byte mutations derived from a known legal parent PDF.
//!
//! The mutated region is one contiguous byte range. It may be longer than a
//! single byte — several pinned malformed values in
//! `docs/03-corpus-requirements.md` (`/Rotate 45`, a degenerate `Tm`, a `null`
//! MediaBox component) simply are not reachable one byte at a time — but it
//! must keep the file length unchanged, or every xref offset behind it shifts
//! and the fixture fails as a broken xref instead of as the declared failure.

use anyhow::{Context, Result, ensure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationSpec<'a> {
    pub parent_fixture_id: &'a str,
    pub byte_offset: usize,
    pub expected_bytes: &'a [u8],
    pub replacement_bytes: &'a [u8],
    pub semantics: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRecord {
    pub parent_fixture_id: String,
    pub byte_offset: u64,
    pub original_bytes: Vec<u8>,
    pub replacement_bytes: Vec<u8>,
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
    ensure!(
        !spec.expected_bytes.is_empty(),
        "mutation range must not be empty"
    );
    ensure!(
        spec.expected_bytes.len() == spec.replacement_bytes.len(),
        "mutation must preserve file length: {} bytes in, {} bytes out",
        spec.expected_bytes.len(),
        spec.replacement_bytes.len()
    );
    ensure!(
        spec.replacement_bytes != spec.expected_bytes,
        "replacement bytes must differ from the parent at offset {}",
        spec.byte_offset
    );
    let end = spec
        .byte_offset
        .checked_add(spec.expected_bytes.len())
        .context("mutation range overflows")?;
    let original = parent.get(spec.byte_offset..end).with_context(|| {
        format!(
            "mutation range {}..{end} is outside parent",
            spec.byte_offset
        )
    })?;
    ensure!(
        original == spec.expected_bytes,
        "parent byte drift at offset {}: expected {:?}, found {original:?}",
        spec.byte_offset,
        spec.expected_bytes
    );

    let mut bytes = parent.to_vec();
    bytes[spec.byte_offset..end].copy_from_slice(spec.replacement_bytes);
    Ok(DerivedFixture {
        bytes,
        record: MutationRecord {
            parent_fixture_id: spec.parent_fixture_id.to_string(),
            byte_offset: u64::try_from(spec.byte_offset).context("mutation offset exceeds u64")?,
            original_bytes: original.to_vec(),
            replacement_bytes: spec.replacement_bytes.to_vec(),
            semantics: spec.semantics.to_string(),
        },
    })
}

/// Verify that a committed child differs from its parent only inside the one
/// contiguous range described by `record`.
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
    ensure!(
        record.original_bytes.len() == record.replacement_bytes.len(),
        "recorded mutation is not length preserving"
    );
    let offset = usize::try_from(record.byte_offset).context("mutation offset exceeds usize")?;
    let end = offset
        .checked_add(record.original_bytes.len())
        .context("mutation range overflows")?;
    ensure!(
        parent.get(offset..end) == Some(record.original_bytes.as_slice()),
        "recorded original bytes do not match parent at offset {offset}"
    );
    ensure!(
        child.get(offset..end) == Some(record.replacement_bytes.as_slice()),
        "recorded replacement bytes do not match child at offset {offset}"
    );
    let outside: Vec<usize> = parent
        .iter()
        .zip(child)
        .enumerate()
        .filter_map(|(index, (before, after))| {
            (before != after && !(offset..end).contains(&index)).then_some(index)
        })
        .collect();
    ensure!(
        outside.is_empty(),
        "child has byte changes outside the declared range {offset}..{end}: {outside:?}"
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
                expected_bytes: b"1",
                replacement_bytes: b"Q",
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
                original_bytes: b"1".to_vec(),
                replacement_bytes: b"Q".to_vec(),
                semantics: "glue the numeric operand to operator Q".into(),
            }
        );
    }

    #[test]
    fn derives_a_multi_byte_range_that_keeps_the_file_length() {
        // `/Rotate 90` -> `/Rotate 45`：两个字节都变，单字节合同表达不了。
        let parent = b"<< /Type /Page /Rotate 90 >>";
        let offset = parent.windows(2).position(|bytes| bytes == b"90").unwrap();
        let derived = derive(
            parent,
            MutationSpec {
                parent_fixture_id: "unit-geom-01-rotate-90",
                byte_offset: offset,
                expected_bytes: b"90",
                replacement_bytes: b"45",
                semantics: "make /Rotate a non-multiple of 90",
            },
        )
        .unwrap();

        assert_eq!(derived.bytes.len(), parent.len());
        assert!(derived.bytes.ends_with(b"/Rotate 45 >>"));
        verify(parent, &derived.bytes, &derived.record).unwrap();
    }

    #[test]
    fn rejects_a_replacement_that_would_change_the_file_length() {
        let parent = b"<< /Rotate 90 >>";
        let error = derive(
            parent,
            MutationSpec {
                parent_fixture_id: "unit-geom-01-rotate-90",
                byte_offset: 12,
                expected_bytes: b"90",
                replacement_bytes: b"450",
                semantics: "widen the value",
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("preserve file length"),
            "{error:#}"
        );
    }

    #[test]
    fn rejects_a_child_that_changes_a_byte_outside_the_declared_range() {
        let parent = b"abcdef";
        let child = b"aXcdeY";
        let record = MutationRecord {
            parent_fixture_id: "unit-base-01-single-line".into(),
            byte_offset: 1,
            original_bytes: b"b".to_vec(),
            replacement_bytes: b"X".to_vec(),
            semantics: "change the selected token".into(),
        };

        let error = verify(parent, child, &record).unwrap_err();
        assert!(error.to_string().contains("outside"), "{error:#}");
    }
}
