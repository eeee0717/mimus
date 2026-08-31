//! Pure, engine-independent quality contracts shared by execution and measurement.

/// Derives the maximum readable gap around an inline formula from source facts.
///
/// Callers own sample eligibility. Both production and scorecard pass only positive,
/// finite source word gaps and font sizes under the ADR-0020 sampling contract.
pub fn formula_continuity_limit(
    word_gaps: impl IntoIterator<Item = f64>,
    font_sizes: impl IntoIterator<Item = f64>,
) -> Option<f64> {
    let word_spacing = median(word_gaps.into_iter().collect());
    let em = median(font_sizes.into_iter().collect())?;
    let source_spacing_limit = word_spacing.map_or(0.0, |spacing| 2.0 * spacing);
    Some(source_spacing_limit.max(1.5 * em))
}

/// Returns whether `right` visually follows `left` on the same line within the
/// source-derived continuity limit. Coordinates use PDF bottom-left space.
#[allow(clippy::too_many_arguments)]
pub fn formula_items_are_adjacent(
    left_left: f64,
    left_bottom: f64,
    left_right: f64,
    left_top: f64,
    right_left: f64,
    right_bottom: f64,
    right_right: f64,
    right_top: f64,
    limit: f64,
) -> bool {
    let values = [
        left_left,
        left_bottom,
        left_right,
        left_top,
        right_left,
        right_bottom,
        right_right,
        right_top,
        limit,
    ];
    if values.iter().any(|value| !value.is_finite()) || limit <= 0.0 {
        return false;
    }
    let same_line = left_top > right_bottom + 0.01 && right_top > left_bottom + 0.01;
    let gap = right_left - left_right;
    same_line && gap >= -0.01 && gap <= limit + 0.01
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|value| value.is_finite() && *value > 0.0);
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[middle - 1] + values[middle]) / 2.0)
    } else {
        Some(values[middle])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity_limit_uses_twice_median_spacing_or_one_and_a_half_em() {
        assert_eq!(
            formula_continuity_limit([5.0, 7.0, 100.0], [10.0]),
            Some(15.0)
        );
        assert_eq!(formula_continuity_limit([8.0, 10.0], [10.0]), Some(18.0));
        assert_eq!(formula_continuity_limit([], [10.0]), Some(15.0));
        assert_eq!(formula_continuity_limit([8.0], []), None);
    }

    #[test]
    fn adjacency_requires_visual_overlap_and_the_shared_gap_limit() {
        assert!(formula_items_are_adjacent(
            100.0, 98.0, 108.0, 110.0, 109.0, 98.0, 117.0, 110.0, 18.0,
        ));
        assert!(!formula_items_are_adjacent(
            100.0, 98.0, 108.0, 110.0, 127.0, 98.0, 135.0, 110.0, 18.0,
        ));
        assert!(!formula_items_are_adjacent(
            100.0, 98.0, 108.0, 110.0, 109.0, 80.0, 117.0, 90.0, 18.0,
        ));
    }
}
