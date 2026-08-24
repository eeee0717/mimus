use std::collections::BTreeSet;

use lopdf::{DecompressError, Dictionary, Document, Object, ObjectId, Stream};

const MAX_REFERENCE_DEPTH: usize = 32;

pub(crate) fn decode(
    document: &Document,
    stream: &Stream,
    max_output: usize,
) -> lopdf::Result<Vec<u8>> {
    let mut resolved = stream.clone();
    for key in [b"Filter".as_slice(), b"DecodeParms".as_slice()] {
        let Ok(value) = stream.dict.get(key) else {
            continue;
        };
        resolved.dict.set(
            key,
            resolve_value(document, value, &mut BTreeSet::new(), 0)?,
        );
    }
    let filters = match resolved.dict.get(b"Filter") {
        Err(_) => return resolved.decompressed_content_with_limit(max_output),
        Ok(Object::Name(name)) => vec![name.clone()],
        Ok(Object::Array(values)) => values
            .iter()
            .map(|value| value.as_name().map(Vec::from))
            .collect::<lopdf::Result<Vec<_>>>()?,
        Ok(_) => {
            return Err(lopdf::Error::InvalidStream(
                "Filter must resolve to a name or an array of names".to_string(),
            ));
        }
    };
    let decode_parameters = decode_parameters(&resolved.dict, filters.len())?;
    let mut decoded = resolved.content;
    for (filter, parameters) in filters.into_iter().zip(decode_parameters) {
        decoded = if matches!(filter.as_slice(), b"ASCIIHexDecode" | b"AHx") {
            decode_ascii_hex(&decoded, max_output)?
        } else {
            let mut dictionary = Dictionary::new();
            dictionary.set("Filter", Object::Name(filter));
            if let Some(parameters) = parameters {
                dictionary.set("DecodeParms", Object::Dictionary(parameters));
            }
            Stream::new(dictionary, decoded).decompressed_content_with_limit(max_output)?
        };
    }
    Ok(decoded)
}

fn decode_parameters(
    dictionary: &Dictionary,
    filter_count: usize,
) -> lopdf::Result<Vec<Option<Dictionary>>> {
    let Ok(parameters) = dictionary.get(b"DecodeParms") else {
        return Ok(vec![None; filter_count]);
    };
    match parameters {
        Object::Null => Ok(vec![None; filter_count]),
        Object::Dictionary(parameters) if filter_count == 1 => Ok(vec![Some(parameters.clone())]),
        Object::Array(parameters) if parameters.len() == filter_count => parameters
            .iter()
            .map(|parameter| match parameter {
                Object::Null => Ok(None),
                Object::Dictionary(dictionary) => Ok(Some(dictionary.clone())),
                _ => Err(lopdf::Error::InvalidStream(
                    "DecodeParms array entries must be dictionaries or null".to_string(),
                )),
            })
            .collect(),
        _ => Err(lopdf::Error::InvalidStream(
            "DecodeParms must match the Filter entry".to_string(),
        )),
    }
}

fn decode_ascii_hex(input: &[u8], max_output: usize) -> lopdf::Result<Vec<u8>> {
    let mut output = Vec::with_capacity((input.len() / 2).min(max_output));
    let mut high_nibble = None;
    for &byte in input {
        if byte == b'>' {
            break;
        }
        if matches!(byte, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' ') {
            continue;
        }
        let nibble = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => {
                return Err(lopdf::Error::InvalidStream(format!(
                    "ASCIIHexDecode contains non-hexadecimal byte 0x{byte:02x}"
                )));
            }
        };
        if let Some(high) = high_nibble.take() {
            push_bounded(&mut output, (high << 4) | nibble, max_output)?;
        } else {
            high_nibble = Some(nibble);
        }
    }
    if let Some(high) = high_nibble {
        push_bounded(&mut output, high << 4, max_output)?;
    }
    Ok(output)
}

fn push_bounded(output: &mut Vec<u8>, byte: u8, max_output: usize) -> lopdf::Result<()> {
    if output.len() >= max_output {
        return Err(DecompressError::MemoryLimitExceeded { limit: max_output }.into());
    }
    output.push(byte);
    Ok(())
}

fn resolve_value(
    document: &Document,
    value: &Object,
    active: &mut BTreeSet<ObjectId>,
    depth: usize,
) -> lopdf::Result<Object> {
    if depth >= MAX_REFERENCE_DEPTH {
        return Err(lopdf::Error::ReferenceLimit);
    }
    match value {
        Object::Reference(object_id) => {
            if !active.insert(*object_id) {
                return Err(lopdf::Error::ReferenceCycle(*object_id));
            }
            let result = resolve_value(
                document,
                document.get_object(*object_id)?,
                active,
                depth + 1,
            );
            active.remove(object_id);
            result
        }
        Object::Array(values) => values
            .iter()
            .map(|value| resolve_value(document, value, active, depth + 1))
            .collect::<lopdf::Result<Vec<_>>>()
            .map(Object::Array),
        Object::Dictionary(dictionary) => {
            let mut resolved = dictionary.clone();
            for (key, value) in dictionary.iter() {
                resolved.set(
                    key.clone(),
                    resolve_value(document, value, active, depth + 1)?,
                );
            }
            Ok(Object::Dictionary(resolved))
        }
        _ => Ok(value.clone()),
    }
}

#[cfg(test)]
mod tests {
    use lopdf::{Dictionary, Object, Stream};

    use super::*;

    #[test]
    fn decodes_an_indirect_filter_name() {
        let mut document = Document::new();
        let mut stream = Stream::new(Dictionary::new(), vec![b'A'; 512]);
        stream.compress().unwrap();
        assert!(stream.dict.has(b"Filter"));
        let filter = stream.dict.remove(b"Filter").unwrap();
        let filter_id = document.add_object(filter);
        stream.dict.set("Filter", filter_id);

        assert_eq!(decode(&document, &stream, 1024).unwrap(), vec![b'A'; 512]);
    }

    #[test]
    fn decodes_ascii_hex_through_an_indirect_filter_name() {
        let mut document = Document::new();
        let filter_id = document.add_object(Object::Name(b"ASCIIHexDecode".to_vec()));
        let stream = Stream::new(
            lopdf::dictionary! { "Filter" => filter_id },
            b"4d 49 4D55 53>ignored".to_vec(),
        );

        assert_eq!(decode(&document, &stream, 5).unwrap(), b"MIMUS");
    }

    #[test]
    fn ascii_hex_rejects_bad_digits_and_honours_the_output_bound() {
        assert!(decode_ascii_hex(b"4G>", 1).is_err());
        assert!(matches!(
            decode_ascii_hex(b"0001>", 1),
            Err(lopdf::Error::Decompress(
                DecompressError::MemoryLimitExceeded { limit: 1 }
            ))
        ));
    }

    #[test]
    fn rejects_a_filter_reference_cycle() {
        let mut document = Document::new();
        document.objects.insert((1, 0), Object::Reference((2, 0)));
        document.objects.insert((2, 0), Object::Reference((1, 0)));
        let stream = Stream::new(lopdf::dictionary! { "Filter" => (1, 0) }, Vec::new());

        assert!(decode(&document, &stream, 1024).is_err());
    }
}
