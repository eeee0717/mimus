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
    let gap = right_left - left_right;
    formula_items_share_line(left_bottom, left_top, right_bottom, right_top)
        && gap >= -0.01
        && gap <= limit + 0.01
}

/// Returns whether two formula-flow items have overlapping vertical extents.
///
/// The relation is invariant to top-left versus bottom-left page coordinates as
/// long as each caller passes the lesser and greater extent consistently.
pub fn formula_items_share_line(
    left_bottom: f64,
    left_top: f64,
    right_bottom: f64,
    right_top: f64,
) -> bool {
    let values = [left_bottom, left_top, right_bottom, right_top];
    values.iter().all(|value| value.is_finite())
        && left_top > right_bottom + 0.01
        && right_top > left_bottom + 0.01
}

/// Extracts the conservative numeric, reference, and unit tokens that must be
/// preserved by translation. Tokens are normalized only when the source and
/// target spellings are lexically explicit equivalents.
pub fn conserved_tokens(source: &str) -> Vec<String> {
    const UNITS: &[&str] = &[
        "GHz", "MHz", "kHz", "dpi", "bits", "bytes", "bit", "byte", "km", "cm", "mm", "ms", "Hz",
        "KB", "MB", "GB", "TB", "mV", "mA", "kW", "dB", "px", "pt", "°C", "min", "s", "h", "m",
        "K", "V", "A", "W",
    ];
    let chars = source.char_indices().collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (_, c) = chars[i];
        if is_chinese_numeral(c) {
            let denominator_start = i;
            let mut separator = i;
            while separator < chars.len() && is_chinese_numeral(chars[separator].1) {
                separator += 1;
            }
            if separator + 2 <= chars.len()
                && chars.get(separator).is_some_and(|value| value.1 == '分')
                && chars
                    .get(separator + 1)
                    .is_some_and(|value| value.1 == '之')
            {
                let numerator_start = separator + 2;
                let mut end = numerator_start;
                while end < chars.len() && is_chinese_numeral(chars[end].1) {
                    end += 1;
                }
                let denominator = parse_chinese_integer(
                    &source[chars[denominator_start].0..char_start(source, &chars, separator)],
                );
                let numerator = parse_chinese_integer(
                    &source[char_start(source, &chars, numerator_start)
                        ..char_start(source, &chars, end)],
                );
                if let (Some(numerator), Some(denominator)) = (numerator, denominator)
                    && denominator != 0
                {
                    out.push(normalize_fraction(numerator, denominator));
                    i = end;
                    continue;
                }
            }
        }
        if c == '[' {
            let mut j = i + 1;
            let mut digit = false;
            while j < chars.len() && (chars[j].1.is_ascii_digit() || ", ".contains(chars[j].1)) {
                digit |= chars[j].1.is_ascii_digit();
                j += 1;
            }
            if digit && j < chars.len() && chars[j].1 == ']' {
                out.push(source[chars[i].0..char_end(source, &chars, j)].to_string());
                i = j + 1;
                continue;
            }
        }
        let signed = "+-−".contains(c)
            && i + 1 < chars.len()
            && chars[i + 1].1.is_ascii_digit()
            && (i == 0 || !chars[i - 1].1.is_alphanumeric());
        if c.is_ascii_digit() || signed {
            let start = i;
            let mut j = i + usize::from(signed);
            while j < chars.len() && chars[j].1.is_ascii_digit() {
                j += 1;
            }
            while j + 3 < chars.len()
                && chars[j].1 == ','
                && chars[j + 1..j + 4]
                    .iter()
                    .all(|value| value.1.is_ascii_digit())
                && chars
                    .get(j + 4)
                    .is_none_or(|value| !value.1.is_ascii_digit())
            {
                j += 4;
            }
            if j < chars.len()
                && chars[j].1 == '.'
                && j + 1 < chars.len()
                && chars[j + 1].1.is_ascii_digit()
            {
                j += 1;
                while j < chars.len() && chars[j].1.is_ascii_digit() {
                    j += 1;
                }
            }
            if j < chars.len() && matches!(chars[j].1, 'e' | 'E') {
                let exponent = j;
                j += 1;
                if j < chars.len() && "+-−".contains(chars[j].1) {
                    j += 1;
                }
                let digits = j;
                while j < chars.len() && chars[j].1.is_ascii_digit() {
                    j += 1;
                }
                if j == digits {
                    j = exponent;
                }
            }
            if j < chars.len() && chars[j].1 == '%' {
                j += 1;
            }
            let raw_number = &source[chars[start].0..char_start(source, &chars, j)];
            if !raw_number.contains(['.', 'e', 'E', '%'])
                && j + 1 < chars.len()
                && chars[j].1 == '/'
                && chars[j + 1].1.is_ascii_digit()
            {
                let mut end = j + 1;
                while end < chars.len() && chars[end].1.is_ascii_digit() {
                    end += 1;
                }
                let numerator = raw_number.replace([',', '−'], "").parse::<u64>();
                let denominator =
                    source[chars[j + 1].0..char_start(source, &chars, end)].parse::<u64>();
                if let (Ok(numerator), Ok(denominator)) = (numerator, denominator)
                    && denominator != 0
                {
                    out.push(normalize_fraction(numerator, denominator));
                    i = end;
                    continue;
                }
            }
            let magnitude = magnitude_power(source, &chars, j);
            out.push(normalize_decimal(raw_number, magnitude.unwrap_or(0)));
            let mut unit_start = j;
            while unit_start < chars.len() && chars[unit_start].1.is_whitespace() {
                unit_start += 1;
            }
            let tail = &source[char_start(source, &chars, unit_start)..];
            if magnitude.is_none()
                && let Some(unit) = UNITS.iter().find(|unit| {
                    tail.starts_with(**unit)
                        && tail[unit.len()..]
                            .chars()
                            .next()
                            .is_none_or(|next| !next.is_ascii_alphabetic())
                })
            {
                out.push((*unit).to_string());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// Returns the source token multiset entries not represented in the target.
/// The result is sorted by normalized token and preserves missing multiplicity.
pub fn missing_conserved_tokens(source: &str, target: &str) -> Vec<String> {
    let mut target_counts = std::collections::BTreeMap::<String, usize>::new();
    for token in conserved_tokens(target) {
        *target_counts.entry(token).or_default() += 1;
    }
    let mut source_counts = std::collections::BTreeMap::<String, usize>::new();
    for token in conserved_tokens(source) {
        *source_counts.entry(token).or_default() += 1;
    }
    let mut missing = Vec::new();
    for (token, expected) in source_counts {
        let found = target_counts.get(&token).copied().unwrap_or(0);
        missing.extend(std::iter::repeat_n(token, expected.saturating_sub(found)));
    }
    missing
}

fn magnitude_power(source: &str, chars: &[(usize, char)], number_end: usize) -> Option<usize> {
    if let Some(suffix) = chars.get(number_end).map(|value| value.1)
        && matches!(suffix, 'K' | 'M' | 'B')
        && chars
            .get(number_end + 1)
            .is_none_or(|value| !value.1.is_ascii_alphabetic())
    {
        return Some(match suffix {
            'K' => 3,
            'M' => 6,
            'B' => 9,
            _ => unreachable!(),
        });
    }
    let mut tail_start = number_end;
    while tail_start < chars.len() && chars[tail_start].1.is_whitespace() {
        tail_start += 1;
    }
    let tail = &source[char_start(source, chars, tail_start)..];
    for (word, power) in [("thousand", 3), ("million", 6), ("billion", 9)] {
        if tail
            .get(..word.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(word))
            && tail[word.len()..]
                .chars()
                .next()
                .is_none_or(|next| !next.is_ascii_alphabetic())
        {
            return Some(power);
        }
    }
    match tail.chars().next() {
        Some('万') => Some(4),
        Some('亿') => Some(8),
        _ => None,
    }
}

fn normalize_decimal(raw: &str, magnitude_power: usize) -> String {
    if raw.contains(['e', 'E', '%']) {
        return raw.replace('−', "-").replace(',', "");
    }
    let normalized = raw.replace('−', "-").replace(',', "");
    let (negative, unsigned) = normalized
        .strip_prefix('-')
        .map_or((false, normalized.as_str()), |value| (true, value));
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let mut digits = format!("{integer}{fraction}");
    while digits.starts_with('0') && digits.len() > 1 {
        digits.remove(0);
    }
    let decimal_places = fraction.len();
    let mut value = if magnitude_power >= decimal_places {
        digits.push_str(&"0".repeat(magnitude_power - decimal_places));
        digits
    } else {
        let split = digits
            .len()
            .saturating_sub(decimal_places - magnitude_power);
        if split == 0 {
            format!(
                "0.{}{}",
                "0".repeat(decimal_places - magnitude_power - digits.len()),
                digits
            )
        } else {
            format!("{}.{}", &digits[..split], &digits[split..])
        }
    };
    if value.contains('.') {
        while value.ends_with('0') {
            value.pop();
        }
        if value.ends_with('.') {
            value.pop();
        }
    }
    if negative && value != "0" {
        value.insert(0, '-');
    }
    value
}

fn normalize_fraction(numerator: u64, denominator: u64) -> String {
    let divisor = gcd(numerator, denominator);
    let numerator = numerator / divisor;
    let denominator = denominator / divisor;
    if denominator == 100 {
        format!("{numerator}%")
    } else {
        format!("{numerator}/{denominator}")
    }
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left.max(1)
}

fn is_chinese_numeral(value: char) -> bool {
    matches!(
        value,
        '零' | '〇'
            | '一'
            | '二'
            | '两'
            | '三'
            | '四'
            | '五'
            | '六'
            | '七'
            | '八'
            | '九'
            | '十'
            | '百'
            | '千'
            | '万'
            | '亿'
    )
}

fn parse_chinese_integer(value: &str) -> Option<u64> {
    let mut total = 0_u64;
    let mut section = 0_u64;
    let mut digit = None;
    for character in value.chars() {
        if let Some(value) = match character {
            '零' | '〇' => Some(0),
            '一' => Some(1),
            '二' | '两' => Some(2),
            '三' => Some(3),
            '四' => Some(4),
            '五' => Some(5),
            '六' => Some(6),
            '七' => Some(7),
            '八' => Some(8),
            '九' => Some(9),
            _ => None,
        } {
            digit = Some(value);
            continue;
        }
        let unit = match character {
            '十' => 10,
            '百' => 100,
            '千' => 1_000,
            '万' => 10_000,
            '亿' => 100_000_000,
            _ => return None,
        };
        if unit < 10_000 {
            section = section.checked_add(digit.take().unwrap_or(1) * unit)?;
        } else {
            section = section.checked_add(digit.take().unwrap_or(0))?;
            total = total.checked_add(section.max(1).checked_mul(unit)?)?;
            section = 0;
        }
    }
    total.checked_add(section)?.checked_add(digit.unwrap_or(0))
}

fn char_start(source: &str, chars: &[(usize, char)], index: usize) -> usize {
    chars.get(index).map_or(source.len(), |value| value.0)
}

fn char_end(source: &str, chars: &[(usize, char)], index: usize) -> usize {
    char_start(source, chars, index + 1)
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
        assert!(formula_items_share_line(577.0, 585.0, 580.0, 584.0));
        assert!(!formula_items_share_line(577.0, 585.0, 588.0, 597.0));
    }

    #[test]
    fn conservation_tokens_cover_exact_numbers_references_and_units() {
        assert_eq!(
            conserved_tokens("At 3.5 days, 20 ms, 1e-3%, see [4,27,28,22]."),
            ["3.5", "20", "ms", "1e-3%", "[4,27,28,22]"]
        );
        assert!(conserved_tokens("one percent and model seven").is_empty());
    }

    #[test]
    fn conservation_tokens_normalize_explicit_localized_quantities() {
        let source = conserved_tokens("36M, 4.5 million, 1/4, and 40K");
        let translated = conserved_tokens("3600 万、450 万、四分之一和 40,000");
        assert_eq!(source, ["36000000", "4500000", "1/4", "40000"]);
        assert_eq!(translated, source);
        assert!(!conserved_tokens("40").contains(&"4".to_owned()));
    }

    #[test]
    fn missing_conservation_tokens_preserve_multiset_counts() {
        assert_eq!(
            missing_conserved_tokens("4 then 4 and [7]", "4 and [7]"),
            ["4"]
        );
        assert!(missing_conserved_tokens("36M", "3600 万").is_empty());
    }
}
