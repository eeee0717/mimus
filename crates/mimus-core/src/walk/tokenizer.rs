use std::ops::Range;

use crate::error::{InputReason, MimusError};
use crate::event::PageDegradeReason;

type Result<T> = std::result::Result<T, TokenizeFailure>;

#[derive(Debug)]
pub(super) struct TokenizeFailure {
    pub reason: PageDegradeReason,
    message: String,
}

impl TokenizeFailure {
    pub fn into_mimus_error(self) -> MimusError {
        MimusError::input(InputReason::OperatorWalk, self.message)
    }
}

pub(super) const MAX_NESTING: usize = 128;
const MAX_INLINE_IMAGE_SCAN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Token {
    pub kind: TokenKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompositeDelimiter {
    ArrayStart,
    ArrayEnd,
    DictionaryStart,
    DictionaryEnd,
    ProcedureStart,
    ProcedureEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InlineImageLengthSource {
    Declared,
    Computed,
    EiScan,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum TokenKind {
    Number(f64),
    Name(Vec<u8>),
    Bytes(Vec<u8>),
    CompositeDelimiter(CompositeDelimiter),
    InlineImage {
        payload_bytes: usize,
        length_source: InlineImageLengthSource,
    },
    Operator(Vec<u8>),
}

pub(super) fn tokenize(input: &[u8]) -> Result<Vec<Token>> {
    Tokenizer {
        input,
        cursor: 0,
        composites: Vec::new(),
    }
    .tokenize()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompositeKind {
    Array,
    Dictionary,
    Procedure,
}

struct Tokenizer<'a> {
    input: &'a [u8],
    cursor: usize,
    composites: Vec<CompositeKind>,
}

impl Tokenizer<'_> {
    fn tokenize(mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        while self.cursor < self.input.len() {
            self.skip_space_and_comments();
            if self.cursor == self.input.len() {
                break;
            }
            let start = self.cursor;
            let kind = if self.starts_keyword(b"BI") {
                self.read_inline_image()?
            } else {
                match self.input[self.cursor] {
                    b'/' => TokenKind::Name(self.read_name()?),
                    b'(' => TokenKind::Bytes(self.read_literal_string()?),
                    b'<' if self.input.get(self.cursor + 1) == Some(&b'<') => {
                        self.cursor += 2;
                        self.push_composite(CompositeKind::Dictionary)?;
                        TokenKind::CompositeDelimiter(CompositeDelimiter::DictionaryStart)
                    }
                    b'<' => TokenKind::Bytes(self.read_hex_string()?),
                    b'>' if self.input.get(self.cursor + 1) == Some(&b'>') => {
                        self.cursor += 2;
                        self.pop_composite(CompositeKind::Dictionary)?;
                        TokenKind::CompositeDelimiter(CompositeDelimiter::DictionaryEnd)
                    }
                    b'[' => {
                        self.cursor += 1;
                        self.push_composite(CompositeKind::Array)?;
                        TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayStart)
                    }
                    b']' => {
                        self.cursor += 1;
                        self.pop_composite(CompositeKind::Array)?;
                        TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayEnd)
                    }
                    b'{' => {
                        self.cursor += 1;
                        self.push_composite(CompositeKind::Procedure)?;
                        TokenKind::CompositeDelimiter(CompositeDelimiter::ProcedureStart)
                    }
                    b'}' => {
                        self.cursor += 1;
                        self.pop_composite(CompositeKind::Procedure)?;
                        TokenKind::CompositeDelimiter(CompositeDelimiter::ProcedureEnd)
                    }
                    byte if is_number_start(byte) => self.read_number_or_operator(),
                    _ => TokenKind::Operator(self.read_word().to_vec()),
                }
            };
            if self.cursor <= start {
                return Err(self.error(format!("tokenizer made no progress at byte {start}")));
            }
            tokens.push(Token {
                kind,
                span: start..self.cursor,
            });
        }
        if let Some(kind) = self.composites.last() {
            return Err(self.error(format!(
                "unterminated {} at end of content stream",
                composite_name(*kind)
            )));
        }
        Ok(tokens)
    }

    fn push_composite(&mut self, kind: CompositeKind) -> Result<()> {
        if self.composites.len() >= MAX_NESTING {
            return Err(self.nesting_error());
        }
        self.composites.push(kind);
        Ok(())
    }

    fn pop_composite(&mut self, expected: CompositeKind) -> Result<()> {
        let actual = self.composites.pop().ok_or_else(|| {
            self.error(format!("unmatched {} terminator", composite_name(expected)))
        })?;
        if actual != expected {
            return Err(self.error(format!(
                "{} terminator closes {}",
                composite_name(expected),
                composite_name(actual)
            )));
        }
        Ok(())
    }

    fn read_number_or_operator(&mut self) -> TokenKind {
        let word = self.read_word();
        std::str::from_utf8(word)
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .map_or_else(|| TokenKind::Operator(word.to_vec()), TokenKind::Number)
    }

    fn read_inline_image(&mut self) -> Result<TokenKind> {
        self.cursor += 2;
        let mut width = None;
        let mut height = None;
        let mut bits_per_component = None;
        let mut components = None;
        let mut declared_length = None;
        let mut filtered = false;

        loop {
            self.skip_space_and_comments();
            if self.starts_keyword(b"ID") {
                self.cursor += 2;
                break;
            }
            if self.input.get(self.cursor) != Some(&b'/') {
                return Err(self.error("inline image dictionary key is not a name"));
            }
            let key = self.read_name()?;
            self.skip_space_and_comments();
            let value = self.read_inline_value()?;
            match key.as_slice() {
                b"W" | b"Width" => width = value.nonnegative_integer(),
                b"H" | b"Height" => height = value.nonnegative_integer(),
                b"BPC" | b"BitsPerComponent" => {
                    bits_per_component = value.nonnegative_integer();
                }
                b"L" | b"Length" => declared_length = value.nonnegative_integer(),
                b"F" | b"Filter" => filtered = true,
                b"CS" | b"ColorSpace" => {
                    components = value.name().and_then(color_components);
                }
                _ => {}
            }
        }

        match self.input.get(self.cursor) {
            Some(b'\r') if self.input.get(self.cursor + 1) == Some(&b'\n') => self.cursor += 2,
            Some(byte) if is_whitespace(*byte) => self.cursor += 1,
            _ => return Err(self.error("inline image ID is not followed by whitespace")),
        }
        let payload_start = self.cursor;
        let computed_length = if filtered {
            None
        } else {
            width.and_then(|width| {
                height.and_then(|height| {
                    bits_per_component.and_then(|bits_per_component| {
                        computed_image_payload_bytes(
                            width,
                            height,
                            bits_per_component,
                            components.unwrap_or(1),
                        )
                    })
                })
            })
        };
        let (payload_bytes, length_source) = if let Some(length) = declared_length {
            (length, InlineImageLengthSource::Declared)
        } else if let Some(length) = computed_length {
            (length, InlineImageLengthSource::Computed)
        } else {
            let (payload_end, terminator) = self.find_inline_image_end(payload_start)?;
            self.cursor = terminator + 2;
            return Ok(TokenKind::InlineImage {
                payload_bytes: payload_end - payload_start,
                length_source: InlineImageLengthSource::EiScan,
            });
        };

        self.cursor = payload_start
            .checked_add(payload_bytes)
            .filter(|cursor| *cursor <= self.input.len())
            .ok_or_else(|| self.error("inline image payload exceeds the content stream"))?;
        self.skip_space_and_comments();
        if !self.starts_keyword(b"EI") {
            return Err(self.error("inline image payload is not followed by EI"));
        }
        self.cursor += 2;
        Ok(TokenKind::InlineImage {
            payload_bytes,
            length_source,
        })
    }

    fn read_inline_value(&mut self) -> Result<InlineValue> {
        match self.input.get(self.cursor).copied() {
            Some(b'/') => Ok(InlineValue::Name(self.read_name()?)),
            Some(b'(') => {
                self.read_literal_string()?;
                Ok(InlineValue::Other)
            }
            Some(b'[') => {
                self.skip_balanced_inline_object(b'[', b']')?;
                Ok(InlineValue::Other)
            }
            Some(b'<') if self.input.get(self.cursor + 1) == Some(&b'<') => {
                self.skip_balanced_inline_object(b'<', b'>')?;
                Ok(InlineValue::Other)
            }
            Some(_) => {
                let word = self.read_word();
                let number = std::str::from_utf8(word)
                    .ok()
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite());
                Ok(number.map_or(InlineValue::Other, InlineValue::Number))
            }
            None => Err(self.error("inline image dictionary reaches end of stream")),
        }
    }

    fn skip_balanced_inline_object(&mut self, open: u8, close: u8) -> Result<()> {
        let dictionary = open == b'<';
        self.cursor += if dictionary { 2 } else { 1 };
        let mut depth = 1usize;
        while let Some(byte) = self.input.get(self.cursor).copied() {
            if byte == b'(' {
                self.read_literal_string()?;
                continue;
            }
            if dictionary && byte == b'<' && self.input.get(self.cursor + 1) == Some(&b'<') {
                depth += 1;
                self.cursor += 2;
            } else if dictionary && byte == b'>' && self.input.get(self.cursor + 1) == Some(&b'>') {
                depth -= 1;
                self.cursor += 2;
            } else if !dictionary && byte == open {
                depth += 1;
                self.cursor += 1;
            } else if !dictionary && byte == close {
                depth -= 1;
                self.cursor += 1;
            } else {
                self.cursor += 1;
            }
            if depth == 0 {
                return Ok(());
            }
            if depth > MAX_NESTING {
                return Err(self.nesting_error());
            }
        }
        Err(self.error("unterminated inline image dictionary value"))
    }

    fn find_inline_image_end(&self, payload_start: usize) -> Result<(usize, usize)> {
        let limit = payload_start
            .saturating_add(MAX_INLINE_IMAGE_SCAN)
            .min(self.input.len());
        let mut cursor = payload_start.saturating_add(1);
        while cursor + 1 < limit {
            if &self.input[cursor..cursor + 2] == b"EI"
                && is_whitespace(self.input[cursor - 1])
                && self
                    .input
                    .get(cursor + 2)
                    .is_none_or(|byte| is_delimiter(*byte))
                && plausible_inline_image_continuation(self.input, cursor + 2)
            {
                let mut payload_end = cursor;
                while payload_end > payload_start && is_whitespace(self.input[payload_end - 1]) {
                    payload_end -= 1;
                }
                return Ok((payload_end, cursor));
            }
            cursor += 1;
        }
        Err(self.error("bounded inline image EI scan found no terminator"))
    }

    fn read_name(&mut self) -> Result<Vec<u8>> {
        self.cursor += 1;
        let start = self.cursor;
        while self
            .input
            .get(self.cursor)
            .is_some_and(|value| !is_delimiter(*value))
        {
            self.cursor += 1;
        }
        let raw = &self.input[start..self.cursor];
        let mut decoded = Vec::with_capacity(raw.len());
        let mut index = 0usize;
        while index < raw.len() {
            if raw[index] == b'#' {
                let high = raw.get(index + 1).and_then(|value| hex_value(*value));
                let low = raw.get(index + 2).and_then(|value| hex_value(*value));
                let (Some(high), Some(low)) = (high, low) else {
                    return Err(self.error("invalid hexadecimal escape in PDF name"));
                };
                decoded.push((high << 4) | low);
                index += 3;
            } else {
                decoded.push(raw[index]);
                index += 1;
            }
        }
        Ok(decoded)
    }

    fn read_literal_string(&mut self) -> Result<Vec<u8>> {
        self.cursor += 1;
        let mut depth = 1usize;
        let mut output = Vec::new();
        while let Some(byte) = self.input.get(self.cursor).copied() {
            self.cursor += 1;
            match byte {
                b'(' => {
                    depth += 1;
                    if self.composites.len().saturating_add(depth) > MAX_NESTING {
                        return Err(self.nesting_error());
                    }
                    output.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(output);
                    }
                    output.push(byte);
                }
                b'\\' => self.read_escape(&mut output)?,
                _ => output.push(byte),
            }
        }
        Err(self.error("unterminated PDF literal string"))
    }

    fn read_escape(&mut self, output: &mut Vec<u8>) -> Result<()> {
        let Some(byte) = self.input.get(self.cursor).copied() else {
            return Err(self.error("unterminated escape in PDF literal string"));
        };
        self.cursor += 1;
        match byte {
            b'n' => output.push(b'\n'),
            b'r' => output.push(b'\r'),
            b't' => output.push(b'\t'),
            b'b' => output.push(8),
            b'f' => output.push(12),
            b'(' | b')' | b'\\' => output.push(byte),
            b'\r' => {
                if self.input.get(self.cursor) == Some(&b'\n') {
                    self.cursor += 1;
                }
            }
            b'\n' => {}
            b'0'..=b'7' => {
                let mut value = u16::from(byte - b'0');
                for _ in 0..2 {
                    let Some(next @ b'0'..=b'7') = self.input.get(self.cursor).copied() else {
                        break;
                    };
                    value = value * 8 + u16::from(next - b'0');
                    self.cursor += 1;
                }
                output.push((value & 0xff) as u8);
            }
            _ => output.push(byte),
        }
        Ok(())
    }

    fn read_hex_string(&mut self) -> Result<Vec<u8>> {
        self.cursor += 1;
        let mut nibbles = Vec::new();
        loop {
            let Some(byte) = self.input.get(self.cursor).copied() else {
                return Err(self.error("unterminated PDF hexadecimal string"));
            };
            self.cursor += 1;
            if byte == b'>' {
                break;
            }
            if is_whitespace(byte) {
                continue;
            }
            nibbles
                .push(hex_value(byte).ok_or_else(|| self.error("invalid PDF hexadecimal string"))?);
        }
        if nibbles.len() % 2 == 1 {
            nibbles.push(0);
        }
        Ok(nibbles
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| (pair[0] << 4) | pair[1])
            .collect())
    }

    fn read_word(&mut self) -> &[u8] {
        let start = self.cursor;
        while self
            .input
            .get(self.cursor)
            .is_some_and(|value| !is_delimiter(*value))
        {
            self.cursor += 1;
        }
        if start == self.cursor {
            self.cursor += 1;
        }
        &self.input[start..self.cursor]
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self
                .input
                .get(self.cursor)
                .is_some_and(|value| is_whitespace(*value))
            {
                self.cursor += 1;
            }
            if self.input.get(self.cursor) != Some(&b'%') {
                return;
            }
            while self
                .input
                .get(self.cursor)
                .is_some_and(|value| !matches!(value, b'\r' | b'\n'))
            {
                self.cursor += 1;
            }
        }
    }

    fn starts_keyword(&self, keyword: &[u8]) -> bool {
        self.input[self.cursor..].starts_with(keyword)
            && self
                .input
                .get(self.cursor + keyword.len())
                .is_none_or(|byte| is_delimiter(*byte))
    }

    fn error(&self, message: impl Into<String>) -> TokenizeFailure {
        tokenize_error(
            PageDegradeReason::ContentStreamSyntax,
            format!("{} at byte {}", message.into(), self.cursor),
        )
    }

    fn nesting_error(&self) -> TokenizeFailure {
        tokenize_error(
            PageDegradeReason::NestingTooDeep,
            format!(
                "content nesting exceeds {MAX_NESTING} at byte {}",
                self.cursor
            ),
        )
    }
}

enum InlineValue {
    Number(f64),
    Name(Vec<u8>),
    Other,
}

impl InlineValue {
    fn nonnegative_integer(&self) -> Option<usize> {
        let Self::Number(value) = self else {
            return None;
        };
        (value.is_finite() && *value >= 0.0 && value.fract() == 0.0 && *value <= usize::MAX as f64)
            .then_some(*value as usize)
    }

    fn name(&self) -> Option<&[u8]> {
        match self {
            Self::Name(name) => Some(name),
            Self::Number(_) | Self::Other => None,
        }
    }
}

fn computed_image_payload_bytes(
    width: usize,
    height: usize,
    bits_per_component: usize,
    components: usize,
) -> Option<usize> {
    let row_bits = width
        .checked_mul(bits_per_component)?
        .checked_mul(components)?;
    row_bits.checked_add(7)?.checked_div(8)?.checked_mul(height)
}

fn color_components(name: &[u8]) -> Option<usize> {
    match name {
        b"G" | b"DeviceGray" => Some(1),
        b"RGB" | b"DeviceRGB" => Some(3),
        b"CMYK" | b"DeviceCMYK" => Some(4),
        _ => None,
    }
}

fn plausible_inline_image_continuation(input: &[u8], mut cursor: usize) -> bool {
    for _ in 0..6 {
        skip_space_and_comments(input, &mut cursor);
        if cursor >= input.len() {
            return true;
        }
        if input[cursor] == b'/' {
            cursor += 1;
            read_word_at(input, &mut cursor);
            continue;
        }
        let word = read_word_at(input, &mut cursor);
        if is_known_operator(word) {
            return true;
        }
        if std::str::from_utf8(word)
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .is_none()
        {
            return false;
        }
    }
    false
}

fn skip_space_and_comments(input: &[u8], cursor: &mut usize) {
    loop {
        while input
            .get(*cursor)
            .is_some_and(|value| is_whitespace(*value))
        {
            *cursor += 1;
        }
        if input.get(*cursor) != Some(&b'%') {
            return;
        }
        while input
            .get(*cursor)
            .is_some_and(|value| !matches!(value, b'\r' | b'\n'))
        {
            *cursor += 1;
        }
    }
}

fn read_word_at<'a>(input: &'a [u8], cursor: &mut usize) -> &'a [u8] {
    let start = *cursor;
    while input
        .get(*cursor)
        .is_some_and(|value| !is_delimiter(*value))
    {
        *cursor += 1;
    }
    if start == *cursor {
        *cursor += 1;
    }
    &input[start..*cursor]
}

fn is_known_operator(value: &[u8]) -> bool {
    matches!(
        value,
        b"q" | b"Q"
            | b"cm"
            | b"BT"
            | b"ET"
            | b"Tf"
            | b"Tm"
            | b"Td"
            | b"TD"
            | b"T*"
            | b"Tj"
            | b"TJ"
            | b"Do"
            | b"sh"
            | b"BX"
            | b"EX"
    )
}

const fn is_number_start(value: u8) -> bool {
    matches!(value, b'+' | b'-' | b'.' | b'0'..=b'9')
}

const fn is_whitespace(value: u8) -> bool {
    matches!(value, 0 | 9 | 10 | 12 | 13 | 32)
}

const fn is_delimiter(value: u8) -> bool {
    is_whitespace(value)
        || matches!(
            value,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

const fn composite_name(kind: CompositeKind) -> &'static str {
    match kind {
        CompositeKind::Array => "array",
        CompositeKind::Dictionary => "dictionary",
        CompositeKind::Procedure => "procedure",
    }
}

fn tokenize_error(reason: PageDegradeReason, message: impl Into<String>) -> TokenizeFailure {
    TokenizeFailure {
        reason,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_the_single_line_program_and_decodes_escapes() {
        let tokens = tokenize(b"BT /F#31 12 Tf 1 0 0 1 72 120 Tm (MI\\115US) Tj ET").unwrap();
        assert!(matches!(&tokens[1].kind, TokenKind::Name(name) if name == b"F1"));
        assert!(matches!(&tokens[11].kind, TokenKind::Bytes(value) if value == b"MIMUS"));
    }

    #[test]
    fn tokenizes_composite_delimiters_as_operands_until_their_operator() {
        let tokens = tokenize(b"/Span<</MCID 0>>BDC [(M)]TJ").unwrap();
        assert!(matches!(
            tokens[1].kind,
            TokenKind::CompositeDelimiter(CompositeDelimiter::DictionaryStart)
        ));
        assert!(matches!(
            tokens[4].kind,
            TokenKind::CompositeDelimiter(CompositeDelimiter::DictionaryEnd)
        ));
        assert!(matches!(&tokens[5].kind, TokenKind::Operator(value) if value == b"BDC"));
        assert!(matches!(
            tokens[6].kind,
            TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayStart)
        ));
        assert!(matches!(
            tokens[8].kind,
            TokenKind::CompositeDelimiter(CompositeDelimiter::ArrayEnd)
        ));
        assert!(matches!(&tokens[9].kind, TokenKind::Operator(value) if value == b"TJ"));
    }

    #[test]
    fn nesting_is_bounded_and_unterminated_composites_fail_at_the_stream_boundary() {
        let too_deep = format!(
            "{}{}",
            "[".repeat(MAX_NESTING + 1),
            "]".repeat(MAX_NESTING + 1)
        );
        assert!(tokenize(too_deep.as_bytes()).is_err());
        assert!(tokenize(b"[(M)").is_err());
    }

    #[test]
    fn inline_image_length_sources_skip_false_ei_bytes() {
        let computed = tokenize(b"BI /W 9 /H 2 /BPC 1 /CS /G ID\n EI \nEI Q").unwrap();
        assert!(matches!(
            computed[0].kind,
            TokenKind::InlineImage {
                payload_bytes: 4,
                length_source: InlineImageLengthSource::Computed
            }
        ));
        assert!(matches!(&computed[1].kind, TokenKind::Operator(value) if value == b"Q"));

        let declared = tokenize(b"BI /W 8 /H 1 /BPC 8 /CS /G /L 8 ID\nABCDEFGH\nEI Q").unwrap();
        assert!(matches!(
            declared[0].kind,
            TokenKind::InlineImage {
                payload_bytes: 8,
                length_source: InlineImageLengthSource::Declared
            }
        ));

        let scanned = tokenize(b"BI /F /AHx ID\n6060>\nEI /S1 sh").unwrap();
        assert!(matches!(
            scanned[0].kind,
            TokenKind::InlineImage {
                length_source: InlineImageLengthSource::EiScan,
                ..
            }
        ));
    }
}
