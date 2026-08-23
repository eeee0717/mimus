use std::ops::Range;

use crate::error::{ErrorReason, MimusError, Result};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Token {
    pub kind: TokenKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum TokenKind {
    Number(f64),
    Name(Vec<u8>),
    Bytes(Vec<u8>),
    Operator(Vec<u8>),
}

pub(super) fn tokenize(input: &[u8]) -> Result<Vec<Token>> {
    let mut cursor = 0;
    let mut tokens = Vec::new();
    while cursor < input.len() {
        skip_space_and_comments(input, &mut cursor);
        if cursor == input.len() {
            break;
        }
        let start = cursor;
        let kind = match input[cursor] {
            b'/' => TokenKind::Name(read_name(input, &mut cursor)?),
            b'(' => TokenKind::Bytes(read_literal_string(input, &mut cursor)?),
            b'<' if input.get(cursor + 1) != Some(&b'<') => {
                TokenKind::Bytes(read_hex_string(input, &mut cursor)?)
            }
            byte if is_number_start(byte) => {
                let word = read_word(input, &mut cursor);
                let number = std::str::from_utf8(word)
                    .ok()
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| walk_error(format!("invalid number at byte {start}")))?;
                TokenKind::Number(number)
            }
            _ => TokenKind::Operator(read_word(input, &mut cursor).to_vec()),
        };
        if cursor == start {
            return Err(walk_error(format!(
                "tokenizer made no progress at byte {start}"
            )));
        }
        tokens.push(Token {
            kind,
            span: start..cursor,
        });
    }
    Ok(tokens)
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

fn read_name(input: &[u8], cursor: &mut usize) -> Result<Vec<u8>> {
    *cursor += 1;
    let start = *cursor;
    while input
        .get(*cursor)
        .is_some_and(|value| !is_delimiter(*value))
    {
        *cursor += 1;
    }
    let raw = &input[start..*cursor];
    let mut decoded = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'#' {
            let high = raw.get(index + 1).and_then(|value| hex_value(*value));
            let low = raw.get(index + 2).and_then(|value| hex_value(*value));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(walk_error("invalid hexadecimal escape in PDF name"));
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

fn read_literal_string(input: &[u8], cursor: &mut usize) -> Result<Vec<u8>> {
    *cursor += 1;
    let mut depth = 1usize;
    let mut output = Vec::new();
    while let Some(byte) = input.get(*cursor).copied() {
        *cursor += 1;
        match byte {
            b'(' => {
                depth += 1;
                output.push(byte);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(output);
                }
                output.push(byte);
            }
            b'\\' => read_escape(input, cursor, &mut output)?,
            _ => output.push(byte),
        }
    }
    Err(walk_error("unterminated PDF literal string"))
}

fn read_escape(input: &[u8], cursor: &mut usize, output: &mut Vec<u8>) -> Result<()> {
    let Some(byte) = input.get(*cursor).copied() else {
        return Err(walk_error("unterminated escape in PDF literal string"));
    };
    *cursor += 1;
    match byte {
        b'n' => output.push(b'\n'),
        b'r' => output.push(b'\r'),
        b't' => output.push(b'\t'),
        b'b' => output.push(8),
        b'f' => output.push(12),
        b'(' | b')' | b'\\' => output.push(byte),
        b'\r' => {
            if input.get(*cursor) == Some(&b'\n') {
                *cursor += 1;
            }
        }
        b'\n' => {}
        b'0'..=b'7' => {
            let mut value = u16::from(byte - b'0');
            for _ in 0..2 {
                let Some(next @ b'0'..=b'7') = input.get(*cursor).copied() else {
                    break;
                };
                value = value * 8 + u16::from(next - b'0');
                *cursor += 1;
            }
            output.push((value & 0xff) as u8);
        }
        _ => output.push(byte),
    }
    Ok(())
}

fn read_hex_string(input: &[u8], cursor: &mut usize) -> Result<Vec<u8>> {
    *cursor += 1;
    let mut nibbles = Vec::new();
    loop {
        let Some(byte) = input.get(*cursor).copied() else {
            return Err(walk_error("unterminated PDF hexadecimal string"));
        };
        *cursor += 1;
        if byte == b'>' {
            break;
        }
        if is_whitespace(byte) {
            continue;
        }
        nibbles.push(hex_value(byte).ok_or_else(|| walk_error("invalid PDF hexadecimal string"))?);
    }
    if nibbles.len() % 2 == 1 {
        nibbles.push(0);
    }
    Ok(nibbles
        .chunks_exact(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect())
}

fn read_word<'a>(input: &'a [u8], cursor: &mut usize) -> &'a [u8] {
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

fn walk_error(message: impl Into<String>) -> MimusError {
    MimusError::input(ErrorReason::OperatorWalk, message)
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
}
