use crate::WalkError;

pub const MAX_NESTING: usize = 128;

#[derive(Debug, Clone)]
pub struct Token {
    pub value: TokenValue,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum TokenValue {
    Number(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Token>),
    InlineImage { payload_bytes: usize },
    Keyword(Vec<u8>),
}

impl Token {
    pub fn number(&self) -> Option<f64> {
        match self.value {
            TokenValue::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn display(&self) -> String {
        match &self.value {
            TokenValue::Number(value) => value.to_string(),
            TokenValue::Name(value) => format!("/{}", String::from_utf8_lossy(value)),
            TokenValue::String(value) => format!("({})", String::from_utf8_lossy(value)),
            TokenValue::Array(value) => format!("[{} items]", value.len()),
            TokenValue::InlineImage { payload_bytes } => {
                format!("inline-image[{payload_bytes} bytes]")
            }
            TokenValue::Keyword(value) => String::from_utf8_lossy(value).into_owned(),
        }
    }
}

pub fn tokenize(input: &[u8], stream_object: u32) -> Result<Vec<Token>, WalkError> {
    let mut tokenizer = Tokenizer {
        input,
        position: 0,
        stream_object,
    };
    tokenizer.sequence(None, 0)
}

struct Tokenizer<'a> {
    input: &'a [u8],
    position: usize,
    stream_object: u32,
}

impl Tokenizer<'_> {
    fn sequence(&mut self, terminator: Option<u8>, depth: usize) -> Result<Vec<Token>, WalkError> {
        if depth > MAX_NESTING {
            return Err(self.error("nesting-too-deep-128", "content nesting exceeds 128"));
        }
        let mut tokens = Vec::new();
        loop {
            self.skip_space_and_comments();
            let Some(&byte) = self.input.get(self.position) else {
                if terminator.is_some() {
                    return Err(self.error("unterminated-array", "array reaches end of stream"));
                }
                return Ok(tokens);
            };
            if Some(byte) == terminator {
                self.position += 1;
                return Ok(tokens);
            }
            tokens.push(self.token(depth)?);
        }
    }

    fn token(&mut self, depth: usize) -> Result<Token, WalkError> {
        let start = self.position;
        let byte = self.input[self.position];
        let value = if self.input[self.position..].starts_with(b"BI")
            && self
                .input
                .get(self.position + 2)
                .is_none_or(|byte| is_delimiter(*byte))
        {
            self.inline_image(depth)?
        } else {
            match byte {
                b'/' => self.name(),
                b'(' => self.literal_string(depth)?,
                b'<' if self.input.get(self.position + 1) != Some(&b'<') => self.hex_string()?,
                b'[' => {
                    self.position += 1;
                    TokenValue::Array(self.sequence(Some(b']'), depth + 1)?)
                }
                b'+' | b'-' | b'.' | b'0'..=b'9' => self.number_or_keyword(),
                _ => self.keyword(),
            }
        };
        Ok(Token {
            value,
            raw: self.input[start..self.position].to_vec(),
        })
    }

    fn inline_image(&mut self, depth: usize) -> Result<TokenValue, WalkError> {
        self.position += 2;
        let mut width = None;
        let mut height = None;
        let mut bits = None;
        let mut components = None;
        let mut length = None;
        loop {
            self.skip_space_and_comments();
            let key = self.token(depth + 1)?;
            if matches!(&key.value, TokenValue::Keyword(value) if value == b"ID") {
                break;
            }
            let TokenValue::Name(key) = key.value else {
                return Err(self.error("inline-image-dictionary", "inline image key is not a name"));
            };
            self.skip_space_and_comments();
            let value = self.token(depth + 1)?;
            match key.as_slice() {
                b"W" | b"Width" => width = value.number().map(|value| value as usize),
                b"H" | b"Height" => height = value.number().map(|value| value as usize),
                b"BPC" | b"BitsPerComponent" => bits = value.number().map(|value| value as usize),
                b"L" | b"Length" => length = value.number().map(|value| value as usize),
                b"CS" | b"ColorSpace" => {
                    components = match value.value {
                        TokenValue::Name(name)
                            if matches!(name.as_slice(), b"G" | b"DeviceGray") =>
                        {
                            Some(1)
                        }
                        TokenValue::Name(name)
                            if matches!(name.as_slice(), b"RGB" | b"DeviceRGB") =>
                        {
                            Some(3)
                        }
                        TokenValue::Name(name)
                            if matches!(name.as_slice(), b"CMYK" | b"DeviceCMYK") =>
                        {
                            Some(4)
                        }
                        _ => None,
                    }
                }
                _ => {}
            }
        }
        match self.input.get(self.position) {
            Some(b'\r') if self.input.get(self.position + 1) == Some(&b'\n') => self.position += 2,
            Some(byte) if byte.is_ascii_whitespace() => self.position += 1,
            _ => {
                return Err(self.error(
                    "inline-image-id",
                    "ID is not followed by one whitespace delimiter",
                ));
            }
        }
        let payload_bytes = length
            .or_else(|| Some((width? * height? * bits? * components.unwrap_or(1)).div_ceil(8)));
        let payload_bytes = payload_bytes.ok_or_else(|| {
            self.error(
                "inline-image-length",
                "cannot derive inline image payload length",
            )
        })?;
        self.position = self
            .position
            .checked_add(payload_bytes)
            .filter(|position| *position <= self.input.len())
            .ok_or_else(|| self.error("inline-image-truncated", "payload exceeds stream"))?;
        self.skip_space_and_comments();
        if !self.input[self.position..].starts_with(b"EI")
            || self
                .input
                .get(self.position + 2)
                .is_some_and(|byte| !is_delimiter(*byte))
        {
            return Err(self.error("inline-image-end", "computed payload is not followed by EI"));
        }
        self.position += 2;
        Ok(TokenValue::InlineImage { payload_bytes })
    }

    fn name(&mut self) -> TokenValue {
        self.position += 1;
        let start = self.position;
        while self.position < self.input.len() && !is_delimiter(self.input[self.position]) {
            self.position += 1;
        }
        TokenValue::Name(self.input[start..self.position].to_vec())
    }

    fn literal_string(&mut self, depth: usize) -> Result<TokenValue, WalkError> {
        self.position += 1;
        let mut nesting = 1usize;
        let mut output = Vec::new();
        while let Some(&byte) = self.input.get(self.position) {
            self.position += 1;
            match byte {
                b'\\' => {
                    let Some(&escaped) = self.input.get(self.position) else {
                        return Err(
                            self.error("unterminated-string", "escape reaches end of stream")
                        );
                    };
                    self.position += 1;
                    match escaped {
                        b'n' => output.push(b'\n'),
                        b'r' => output.push(b'\r'),
                        b't' => output.push(b'\t'),
                        b'b' => output.push(8),
                        b'f' => output.push(12),
                        b'\n' => {}
                        b'\r' => {
                            if self.input.get(self.position) == Some(&b'\n') {
                                self.position += 1;
                            }
                        }
                        other => output.push(other),
                    }
                }
                b'(' => {
                    nesting += 1;
                    if depth + nesting > MAX_NESTING {
                        return Err(
                            self.error("nesting-too-deep-128", "string nesting exceeds 128")
                        );
                    }
                    output.push(byte);
                }
                b')' => {
                    nesting -= 1;
                    if nesting == 0 {
                        return Ok(TokenValue::String(output));
                    }
                    output.push(byte);
                }
                other => output.push(other),
            }
        }
        Err(self.error(
            "unterminated-string",
            "literal string reaches end of stream",
        ))
    }

    fn hex_string(&mut self) -> Result<TokenValue, WalkError> {
        self.position += 1;
        let mut nibbles = Vec::new();
        while let Some(&byte) = self.input.get(self.position) {
            self.position += 1;
            if byte == b'>' {
                if nibbles.len() % 2 == 1 {
                    nibbles.push(0);
                }
                let bytes = nibbles
                    .chunks(2)
                    .map(|pair| pair[0] * 16 + pair[1])
                    .collect();
                return Ok(TokenValue::String(bytes));
            }
            if byte.is_ascii_whitespace() {
                continue;
            }
            nibbles.push(
                hex_nibble(byte).ok_or_else(|| self.error("bad-hex-string", "non-hex digit"))?,
            );
        }
        Err(self.error(
            "unterminated-hex-string",
            "hex string reaches end of stream",
        ))
    }

    fn number_or_keyword(&mut self) -> TokenValue {
        let start = self.position;
        while self.position < self.input.len() && !is_delimiter(self.input[self.position]) {
            self.position += 1;
        }
        let raw = &self.input[start..self.position];
        match std::str::from_utf8(raw)
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
        {
            Some(value) => TokenValue::Number(value),
            None => TokenValue::Keyword(raw.to_vec()),
        }
    }

    fn keyword(&mut self) -> TokenValue {
        let start = self.position;
        while self.position < self.input.len() && !is_delimiter(self.input[self.position]) {
            self.position += 1;
        }
        if self.position == start {
            self.position += 1;
        }
        TokenValue::Keyword(self.input[start..self.position].to_vec())
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self
                .input
                .get(self.position)
                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == 0)
            {
                self.position += 1;
            }
            if self.input.get(self.position) != Some(&b'%') {
                return;
            }
            while self
                .input
                .get(self.position)
                .is_some_and(|byte| *byte != b'\n' && *byte != b'\r')
            {
                self.position += 1;
            }
        }
    }

    fn error(&self, id: &str, detail: &str) -> WalkError {
        WalkError {
            id: id.into(),
            detail: format!(
                "stream {} byte {}: {detail}",
                self.stream_object, self.position
            ),
        }
    }
}

fn is_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            0 | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
