//! Pure, engine-independent quality contracts shared by execution and measurement.

mod agl;

/// Preferred output-font family for one translated Unicode scalar.
///
/// This policy is shared by production typesetting and independent quality
/// measurement. Preference is not exclusive: callers may try the other
/// family when the preferred family does not contain the scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputScriptPreference {
    Cjk,
    Latin,
    Default,
}

/// Classifies a translated scalar for the two-family output-font policy.
#[must_use]
pub const fn output_script_preference(character: char) -> OutputScriptPreference {
    match character as u32 {
        // Chinese-context punctuation must stay with the CJK family even
        // though STIX also covers much of General Punctuation.
        0x2010..=0x2027
        | 0x2e80..=0x2eff
        | 0x2f00..=0x2fdf
        | 0x2ff0..=0x2fff
        | 0x3000..=0x303f
        | 0x3040..=0x309f
        | 0x30a0..=0x30ff
        | 0x3100..=0x312f
        | 0x3130..=0x318f
        | 0x3190..=0x319f
        | 0x31a0..=0x31bf
        | 0x31c0..=0x31ef
        | 0x31f0..=0x31ff
        | 0x3200..=0x32ff
        | 0x3300..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa960..=0xa97f
        | 0xac00..=0xd7af
        | 0xd7b0..=0xd7ff
        | 0xf900..=0xfaff
        | 0xfe10..=0xfe1f
        | 0xfe30..=0xfe4f
        | 0xff00..=0xffef
        | 0x1aff0..=0x1afff
        | 0x1b000..=0x1b16f
        | 0x1f200..=0x1f2ff
        | 0x20000..=0x2ee5f
        | 0x2f800..=0x2fa1f
        | 0x30000..=0x323af => OutputScriptPreference::Cjk,

        // Latin-family text and the technical-symbol blocks for which STIX
        // Two Text is the canonical translated-text face.
        0x0020..=0x007e
        | 0x00a0..=0x02ff
        | 0x0300..=0x036f
        | 0x0370..=0x052f
        | 0x1c80..=0x1c8f
        | 0x1d00..=0x1dbf
        | 0x1e00..=0x1fff
        | 0x2070..=0x209f
        | 0x20d0..=0x20ff
        | 0x2100..=0x214f
        | 0x2190..=0x22ff
        | 0x27c0..=0x2bff
        | 0x2de0..=0x2dff
        | 0xa640..=0xa69f
        | 0xa720..=0xa7ff
        | 0xab30..=0xab6f
        | 0x10780..=0x107bf
        | 0x1d400..=0x1d7ff
        | 0x1df00..=0x1dfff => OutputScriptPreference::Latin,
        _ => OutputScriptPreference::Default,
    }
}

/// Returns whether a translated line may not begin with `character` under the
/// V1 Chinese kinsoku policy.
pub fn forbidden_line_start(character: char) -> bool {
    matches!(
        character,
        '\u{3001}'
            | '\u{3002}'
            | '\u{3009}'
            | '\u{300b}'
            | '\u{300d}'
            | '\u{300f}'
            | '\u{3011}'
            | '\u{3015}'
            | '\u{3017}'
            | '\u{3019}'
            | '\u{301b}'
            | '\u{301e}'
            | '\u{301f}'
            | '\u{2019}'
            | '\u{201d}'
            | '\u{ff01}'
            | '\u{ff09}'
            | '\u{ff0c}'
            | '\u{ff1a}'
            | '\u{ff1b}'
            | '\u{ff1f}'
    )
}

/// Returns whether a translated line may not end with `character` under the
/// V1 Chinese kinsoku policy.
pub fn forbidden_line_end(character: char) -> bool {
    matches!(
        character,
        '\u{3008}'
            | '\u{300a}'
            | '\u{300c}'
            | '\u{300e}'
            | '\u{3010}'
            | '\u{3014}'
            | '\u{3016}'
            | '\u{3018}'
            | '\u{301a}'
            | '\u{2018}'
            | '\u{201c}'
            | '\u{ff08}'
    )
}

/// Resolves an explicit PDF `/Differences` glyph name only when Adobe's
/// legacy Glyph List maps it to exactly one safe Unicode scalar.
///
/// Suffixes follow the AGL production-name rule; composite names are rejected
/// because this recovery path is single-scalar only.
pub fn differences_agl_single_scalar(name: &[u8]) -> Option<char> {
    let base = name.split(|byte| *byte == b'.').next()?;
    if base.is_empty() || base.contains(&b'_') {
        return None;
    }
    let character = agl::single_scalar(base)?;
    (!character.is_control() && !is_unicode_noncharacter(character)).then_some(character)
}

fn is_unicode_noncharacter(character: char) -> bool {
    let value = u32::from(character);
    (0xfdd0..=0xfdef).contains(&value) || value & 0xffff >= 0xfffe
}

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

/// The page-zero visual band in which `text` paragraphs are treated as the
/// complete author block. Coordinates use PDF bottom-left space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleAuthorBand {
    pub lower: f64,
    pub upper: f64,
    pub tolerance: f64,
}

impl TitleAuthorBand {
    /// Returns whether a paragraph lies completely inside the author band.
    pub fn contains(self, paragraph_bottom: f64, paragraph_top: f64) -> bool {
        paragraph_bottom.is_finite()
            && paragraph_top.is_finite()
            && paragraph_bottom <= paragraph_top
            && paragraph_bottom >= self.lower - 0.01
            && paragraph_top <= self.upper + 0.01
    }
}

/// Builds the page-zero title/author band from its two geometric anchors.
///
/// The title's bottom edge is the upper anchor and the abstract or first
/// paragraph-title's top edge is the lower anchor. Half the median anchor font
/// size supplies a line-height-scale tolerance for minor detector/ink overlap.
pub fn title_author_band(
    title_bottom: f64,
    lower_anchor_top: f64,
    anchor_font_sizes: impl IntoIterator<Item = f64>,
) -> Option<TitleAuthorBand> {
    if !title_bottom.is_finite()
        || !lower_anchor_top.is_finite()
        || lower_anchor_top >= title_bottom
    {
        return None;
    }
    let tolerance = median(
        anchor_font_sizes
            .into_iter()
            .filter(|value| value.is_finite() && *value > 0.0)
            .collect(),
    )? * 0.5;
    Some(TitleAuthorBand {
        lower: lower_anchor_top - tolerance,
        upper: title_bottom + tolerance,
        tolerance,
    })
}

/// The output positioning for a retained section-number prefix.
///
/// `gap_pt` is an explicit text advance after the output prefix. `title_left`
/// is the resulting first title-glyph origin in page space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetainedSectionNumberPosition {
    pub gap_pt: f64,
    pub title_left: f64,
    pub clamped: bool,
}

/// Restores the source title origin after a retained section-number prefix.
///
/// The output prefix remains anchored at `source_prefix_left`. Its configured
/// font width can differ from the source prefix width, so the residual advance
/// shrinks or grows accordingly and is clamped to a 0.25em minimum.
pub fn retained_section_number_position(
    source_prefix_left: f64,
    source_title_left: f64,
    output_prefix_width: f64,
    output_font_size: f64,
) -> Option<RetainedSectionNumberPosition> {
    let values = [
        source_prefix_left,
        source_title_left,
        output_prefix_width,
        output_font_size,
    ];
    if values.iter().any(|value| !value.is_finite())
        || source_title_left < source_prefix_left
        || output_prefix_width < 0.0
        || output_font_size <= 0.0
    {
        return None;
    }
    let requested_gap = source_title_left - source_prefix_left - output_prefix_width;
    let minimum_gap = output_font_size * 0.25;
    let clamped = requested_gap < minimum_gap;
    let gap_pt = requested_gap.max(minimum_gap);
    Some(RetainedSectionNumberPosition {
        gap_pt,
        title_left: source_prefix_left + output_prefix_width + gap_pt,
        clamped,
    })
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
    fn output_script_policy_covers_each_documented_family() {
        for value in ['中', '。', '“', '—', 'あ', '한', 'Ａ', '\u{20000}'] {
            assert_eq!(
                output_script_preference(value),
                OutputScriptPreference::Cjk,
                "unexpected class for U+{:04X}",
                value as u32
            );
        }
        for value in ['A', '9', '.', 'Ł', 'ϵ', 'Ж', 'ℏ', '∗', '→', '²'] {
            assert_eq!(
                output_script_preference(value),
                OutputScriptPreference::Latin,
                "unexpected class for U+{:04X}",
                value as u32
            );
        }
        assert_eq!(
            output_script_preference('😀'),
            OutputScriptPreference::Default
        );
    }

    #[test]
    fn title_author_band_uses_geometry_and_half_median_anchor_font_size() {
        let band = title_author_band(90.0, 30.0, [10.0, 14.0]).unwrap();
        assert_eq!(band.tolerance, 6.0);
        assert_eq!((band.lower, band.upper), (24.0, 96.0));
        assert!(band.contains(40.0, 80.0));
        assert!(!band.contains(10.0, 20.0));
        assert!(!band.contains(97.0, 100.0));
    }

    #[test]
    fn title_author_band_rejects_missing_line_height_and_reversed_anchors() {
        assert!(title_author_band(90.0, 30.0, []).is_none());
        assert!(title_author_band(30.0, 90.0, [10.0]).is_none());
    }

    #[test]
    fn retained_section_number_position_restores_source_title_or_clamps_to_quarter_em() {
        let ordinary = retained_section_number_position(72.0, 90.0, 6.0, 12.0).unwrap();
        assert_eq!(ordinary.gap_pt, 12.0);
        assert_eq!(ordinary.title_left, 90.0);
        assert!(!ordinary.clamped);

        let wider_prefix = retained_section_number_position(72.0, 90.0, 10.0, 12.0).unwrap();
        assert_eq!(wider_prefix.gap_pt, 8.0);
        assert_eq!(wider_prefix.title_left, 90.0);
        assert!(!wider_prefix.clamped);

        let clamped = retained_section_number_position(72.0, 90.0, 17.0, 12.0).unwrap();
        assert_eq!(clamped.gap_pt, 3.0);
        assert_eq!(clamped.title_left, 92.0);
        assert!(clamped.clamped);
    }

    #[test]
    fn retained_section_number_position_rejects_invalid_geometry() {
        assert!(retained_section_number_position(90.0, 72.0, 6.0, 12.0).is_none());
        assert!(retained_section_number_position(72.0, 90.0, -1.0, 12.0).is_none());
        assert!(retained_section_number_position(72.0, 90.0, 6.0, 0.0).is_none());
        assert!(retained_section_number_position(f64::NAN, 90.0, 6.0, 12.0).is_none());
    }

    #[test]
    fn v1_kinsoku_set_covers_cjk_closing_and_opening_punctuation_only() {
        for character in "，。、；：！？）」』】》”’".chars() {
            assert!(forbidden_line_start(character), "{character}");
            assert!(!forbidden_line_end(character), "{character}");
        }
        for character in "（「『【《“‘".chars() {
            assert!(forbidden_line_end(character), "{character}");
            assert!(!forbidden_line_start(character), "{character}");
        }
        for character in "-/～—·".chars() {
            assert!(!forbidden_line_start(character), "{character}");
            assert!(!forbidden_line_end(character), "{character}");
        }
    }

    #[test]
    fn differences_agl_accepts_only_single_safe_legacy_scalars() {
        assert_eq!(differences_agl_single_scalar(b"Aacute"), Some('Á'));
        assert_eq!(differences_agl_single_scalar(b"Aacute.alt"), Some('Á'));
        assert_eq!(differences_agl_single_scalar(b"phi1"), Some('\u{03d5}'));
        assert_eq!(differences_agl_single_scalar(b"diamond"), Some('\u{2666}'));
        assert_eq!(differences_agl_single_scalar(b"epsilon1"), None);
        assert_eq!(differences_agl_single_scalar(b"dalethatafpatah"), None);
        assert_eq!(differences_agl_single_scalar(b"f_f_i"), None);
        assert_eq!(differences_agl_single_scalar(b"unknown"), None);
    }

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
