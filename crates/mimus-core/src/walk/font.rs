use std::collections::{BTreeMap, BTreeSet};

use lopdf::{Dictionary, Document, Object, ObjectId};

use super::{MAX_STREAM_BYTES, UnicodeProvenance, object_number};
use crate::il::FontRef;
use crate::pdf_stream;

#[derive(Debug, Clone)]
pub(super) struct DecodedGlyph {
    pub unicode: Vec<char>,
    pub unicode_provenance: UnicodeProvenance,
    pub code: u32,
    pub encoded: Vec<u8>,
    pub advance_em: f64,
    pub font_supported: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedFont {
    pub reference: FontRef,
    pub is_bold: bool,
    pub ascent_em: f64,
    pub descent_em: f64,
    pub normalized_descriptor_descent: bool,
    pub engine_mismatch_tolerated: bool,
    supported: bool,
    unicode: UnicodeSource,
    kind: FontKind,
}

#[derive(Debug, Clone)]
enum FontKind {
    Simple(SimpleFont),
    Composite(CompositeFont),
    Type3(Type3Font),
    Unknown,
}

#[derive(Debug, Clone)]
struct SimpleFont {
    first_char: i64,
    widths: Option<Vec<f64>>,
    missing_width: Option<f64>,
    encoding: SimpleEncoding,
    embedded_unicode: EmbeddedUnicode,
}

#[derive(Debug, Clone)]
struct CompositeFont {
    encoding: CompositeEncoding,
    widths: CidWidths,
    cid_to_gid: CidToGid,
    gid_to_unicode: BTreeMap<u16, char>,
}

#[derive(Debug, Clone)]
enum CompositeEncoding {
    Identity,
    Embedded(CodeMap),
    Unsupported,
}

#[derive(Debug, Clone)]
struct Type3Font {
    encoding: SimpleEncoding,
    glyphs: BTreeMap<Vec<u8>, Type3Glyph>,
}

#[derive(Debug, Clone, Copy)]
struct Type3Glyph {
    advance_em: f64,
}

#[derive(Debug, Clone)]
enum UnicodeSource {
    Absent,
    Valid(UnicodeMap),
    Invalid,
}

#[derive(Debug, Clone)]
enum EmbeddedUnicode {
    Absent,
    Valid(BTreeSet<char>),
    Invalid,
}

#[derive(Debug, Clone, Default)]
struct SimpleEncoding {
    unicode: Vec<Option<char>>,
    glyph_names: Vec<Option<Vec<u8>>>,
    differences_agl: Vec<bool>,
    embedded_type1: Vec<bool>,
}

#[derive(Debug, Clone, Default)]
struct CodeMap {
    codespaces: Vec<CodeSpace>,
    chars: BTreeMap<Vec<u8>, u32>,
    ranges: Vec<CodeRange>,
}

#[derive(Debug, Clone, Default)]
struct UnicodeMap {
    codespaces: Vec<CodeSpace>,
    chars: BTreeMap<Vec<u8>, Vec<char>>,
}

#[derive(Debug, Clone)]
struct CodeSpace {
    low: Vec<u8>,
    high: Vec<u8>,
}

#[derive(Debug, Clone)]
struct CodeRange {
    low: Vec<u8>,
    high: Vec<u8>,
    first: u32,
}

#[derive(Debug, Clone)]
struct CidWidths {
    default: f64,
    explicit: BTreeMap<u32, f64>,
}

#[derive(Debug, Clone)]
enum CidToGid {
    Identity,
    Map(Vec<u8>),
    Invalid,
}

#[derive(Debug)]
struct FontFailure;

type FontResult<T> = std::result::Result<T, FontFailure>;

impl ResolvedFont {
    pub fn resolve(
        document: &Document,
        resources: &Dictionary,
        name: &[u8],
    ) -> std::result::Result<Self, ObjectId> {
        let fallback_reference = FontRef {
            resource_name: resource_name(name),
            object_number: 0,
            generation: 0,
        };
        if let Some(object_id) = dangling_font_reference(document, resources, name) {
            return Err(object_id);
        }
        Ok(
            Self::try_resolve(document, resources, name).unwrap_or(Self {
                reference: fallback_reference,
                is_bold: font_name_is_bold(name),
                ascent_em: 0.0,
                descent_em: 0.0,
                normalized_descriptor_descent: false,
                engine_mismatch_tolerated: true,
                supported: false,
                unicode: UnicodeSource::Absent,
                kind: FontKind::Unknown,
            }),
        )
    }

    fn try_resolve(document: &Document, resources: &Dictionary, name: &[u8]) -> FontResult<Self> {
        let fonts = resources
            .get_deref(b"Font", document)
            .and_then(Object::as_dict)
            .map_err(|_| FontFailure)?;
        let font_object = fonts.get(name).map_err(|_| FontFailure)?;
        let (font, object_id) = dereference_dictionary(document, font_object)?;
        let reference = FontRef {
            resource_name: resource_name(name),
            object_number: object_id.map_or(0, |id| id.0),
            generation: object_id.map_or(0, |id| id.1),
        };
        let subtype = font
            .get(b"Subtype")
            .and_then(Object::as_name)
            .map_err(|_| FontFailure)?;
        match subtype {
            b"Type1" | b"TrueType" => Self::simple(document, font, reference, subtype),
            b"Type0" => Self::composite(document, font, reference),
            b"Type3" => Self::type3(document, font, reference),
            _ => Ok(Self {
                reference,
                is_bold: font_dictionary_is_bold(font, None),
                ascent_em: 0.0,
                descent_em: 0.0,
                normalized_descriptor_descent: false,
                engine_mismatch_tolerated: true,
                supported: false,
                unicode: read_to_unicode(document, font),
                kind: FontKind::Unknown,
            }),
        }
    }

    fn simple(
        document: &Document,
        font: &Dictionary,
        reference: FontRef,
        subtype: &[u8],
    ) -> FontResult<Self> {
        let first_char = font.get(b"FirstChar").and_then(Object::as_i64).unwrap_or(0);
        let widths = font
            .get_deref(b"Widths", document)
            .and_then(Object::as_array)
            .ok()
            .and_then(|items| items.iter().map(object_number).collect::<Option<Vec<_>>>());
        let descriptor = font
            .get_deref(b"FontDescriptor", document)
            .and_then(Object::as_dict)
            .ok();
        let base_font = font
            .get(b"BaseFont")
            .and_then(Object::as_name)
            .unwrap_or_default();
        let standard_14 = is_standard_14_name(base_font);
        let is_bold = font_dictionary_is_bold(font, descriptor);
        let ascent = descriptor
            .and_then(|value| value.get(b"Ascent").ok())
            .and_then(object_number)
            .or_else(|| standard_14.then_some(718.0));
        let raw_descent = descriptor
            .and_then(|value| value.get(b"Descent").ok())
            .and_then(object_number)
            .or_else(|| standard_14.then_some(-207.0));
        let normalized_descriptor_descent = raw_descent.is_some_and(|value| value > 0.0);
        let descent = raw_descent.map(normalize_descriptor_descent);
        let missing_width = descriptor
            .and_then(|value| value.get(b"MissingWidth").ok())
            .and_then(object_number);
        let encoding = if subtype == b"Type1" && !font.has(b"Encoding") {
            match descriptor.and_then(|value| read_embedded_type1_encoding(document, value)) {
                Some(result) => result?,
                None => read_simple_encoding(document, font, false)?,
            }
        } else {
            read_simple_encoding(document, font, subtype == b"TrueType")?
        };
        let embedded_unicode = descriptor.map_or(EmbeddedUnicode::Absent, |value| {
            read_embedded_unicode(document, value)
        });
        let supported = widths.is_some()
            && ascent.is_some()
            && descent.is_some()
            && !matches!(embedded_unicode, EmbeddedUnicode::Invalid);
        Ok(Self {
            reference,
            is_bold,
            ascent_em: ascent.unwrap_or(0.0) / 1000.0,
            descent_em: descent.unwrap_or(0.0) / 1000.0,
            normalized_descriptor_descent,
            engine_mismatch_tolerated: false,
            supported,
            unicode: read_to_unicode(document, font),
            kind: FontKind::Simple(SimpleFont {
                first_char,
                widths,
                missing_width,
                encoding,
                embedded_unicode,
            }),
        })
    }

    fn composite(document: &Document, font: &Dictionary, reference: FontRef) -> FontResult<Self> {
        let unicode = read_to_unicode(document, font);
        let encoding_object = font.get_deref(b"Encoding", document).ok();
        let encoding = match encoding_object {
            Some(Object::Name(name))
                if matches!(
                    name.as_slice(),
                    b"Identity-H"
                        | b"Identity-V"
                        | b"DLIdent-H"
                        | b"DLIdent-V"
                        | b"DLIdentity-H"
                        | b"DLIdentity-V"
                ) =>
            {
                CompositeEncoding::Identity
            }
            Some(Object::Name(_)) => CompositeEncoding::Unsupported,
            Some(Object::Stream(stream)) => pdf_stream::decode(document, stream, MAX_STREAM_BYTES)
                .ok()
                .and_then(|bytes| parse_encoding_cmap(&bytes).ok())
                .map(CompositeEncoding::Embedded)
                .unwrap_or(CompositeEncoding::Unsupported),
            _ => CompositeEncoding::Unsupported,
        };
        let descendants = font
            .get_deref(b"DescendantFonts", document)
            .and_then(Object::as_array)
            .map_err(|_| FontFailure)?;
        let descendant_object = descendants.first().ok_or(FontFailure)?;
        let (descendant, _) = dereference_dictionary(document, descendant_object)?;
        let descendant_subtype = descendant.get(b"Subtype").and_then(Object::as_name).ok();
        let descriptor = descendant
            .get_deref(b"FontDescriptor", document)
            .and_then(Object::as_dict)
            .ok();
        let is_bold =
            font_dictionary_is_bold(font, None) || font_dictionary_is_bold(descendant, descriptor);
        let ascent = descriptor
            .and_then(|value| value.get(b"Ascent").ok())
            .and_then(object_number);
        let raw_descent = descriptor
            .and_then(|value| value.get(b"Descent").ok())
            .and_then(object_number);
        let normalized_descriptor_descent = raw_descent.is_some_and(|value| value > 0.0);
        let descent = raw_descent.map(normalize_descriptor_descent);
        let embedded = descriptor.and_then(|value| embedded_true_type(document, value));
        let gid_to_unicode = embedded
            .as_deref()
            .and_then(|bytes| reverse_unicode_cmap(bytes).ok())
            .unwrap_or_default();
        let cid_to_gid = read_cid_to_gid(document, descendant).unwrap_or(CidToGid::Invalid);
        let widths = read_cid_widths(document, descendant)?;
        let has_reliable_to_unicode = matches!(unicode, UnicodeSource::Valid(_));
        let has_embedded_fallback = descendant_subtype == Some(b"CIDFontType2")
            && embedded.is_some()
            && !gid_to_unicode.is_empty()
            && !matches!(cid_to_gid, CidToGid::Invalid);
        let supported = matches!(descendant_subtype, Some(b"CIDFontType0" | b"CIDFontType2"))
            && ascent.is_some()
            && descent.is_some()
            && !matches!(encoding, CompositeEncoding::Unsupported)
            && (has_reliable_to_unicode || has_embedded_fallback);
        let embedded_unicode = matches!(unicode, UnicodeSource::Absent);
        let identity_alias = matches!(
            encoding_object,
            Some(Object::Name(name)) if matches!(name.as_slice(), b"DLIdent-H" | b"DLIdent-V" | b"DLIdentity-H" | b"DLIdentity-V")
        );
        Ok(Self {
            reference,
            is_bold,
            ascent_em: ascent.unwrap_or(0.0) / 1000.0,
            descent_em: descent.unwrap_or(0.0) / 1000.0,
            normalized_descriptor_descent,
            engine_mismatch_tolerated: embedded_unicode || identity_alias || !supported,
            supported,
            unicode,
            kind: FontKind::Composite(CompositeFont {
                encoding,
                widths,
                cid_to_gid,
                gid_to_unicode,
            }),
        })
    }

    fn type3(document: &Document, font: &Dictionary, reference: FontRef) -> FontResult<Self> {
        let matrix = font
            .get_deref(b"FontMatrix", document)
            .and_then(Object::as_array)
            .ok()
            .and_then(|values| values.iter().map(object_number).collect::<Option<Vec<_>>>());
        let valid_matrix = matrix.as_ref().is_some_and(|values| {
            values.len() == 6
                && values.iter().all(|value| value.is_finite())
                && (values[0] * values[3] - values[1] * values[2]).abs() > 1e-12
        });
        let encoding = read_simple_encoding(document, font, false)?;
        let char_procs = font
            .get_deref(b"CharProcs", document)
            .and_then(Object::as_dict)
            .ok();
        let mut glyphs = BTreeMap::new();
        if let (Some(matrix), Some(char_procs)) = (matrix.as_ref(), char_procs) {
            for name in encoding.glyph_names.iter().flatten() {
                let Some(stream) = char_procs
                    .get_deref(name, document)
                    .ok()
                    .and_then(|object| object.as_stream().ok())
                else {
                    continue;
                };
                let Some((wx, wy)) = pdf_stream::decode(document, stream, MAX_STREAM_BYTES)
                    .ok()
                    .and_then(|bytes| type3_advance(&bytes))
                else {
                    continue;
                };
                let advance_em = wx * matrix[0] + wy * matrix[2];
                glyphs.insert(name.clone(), Type3Glyph { advance_em });
            }
        }
        let bbox = font
            .get_deref(b"FontBBox", document)
            .and_then(Object::as_array)
            .ok()
            .and_then(|values| values.iter().map(object_number).collect::<Option<Vec<_>>>());
        let (descent_em, ascent_em) = match (matrix.as_ref(), bbox.as_ref()) {
            (Some(matrix), Some(bbox)) if matrix.len() == 6 && bbox.len() == 4 => {
                let ys = [
                    bbox[0] * matrix[1] + bbox[1] * matrix[3] + matrix[5],
                    bbox[0] * matrix[1] + bbox[3] * matrix[3] + matrix[5],
                    bbox[2] * matrix[1] + bbox[1] * matrix[3] + matrix[5],
                    bbox[2] * matrix[1] + bbox[3] * matrix[3] + matrix[5],
                ];
                (
                    ys.iter().copied().fold(f64::INFINITY, f64::min),
                    ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                )
            }
            _ => (0.0, 0.0),
        };
        let supported = valid_matrix && !glyphs.is_empty();
        Ok(Self {
            reference,
            is_bold: font_dictionary_is_bold(font, None),
            ascent_em,
            descent_em,
            normalized_descriptor_descent: false,
            engine_mismatch_tolerated: !supported,
            supported,
            unicode: read_to_unicode(document, font),
            kind: FontKind::Type3(Type3Font { encoding, glyphs }),
        })
    }

    pub fn decode(&self, bytes: &[u8]) -> Vec<DecodedGlyph> {
        match &self.kind {
            FontKind::Simple(font) => bytes
                .iter()
                .map(|code| {
                    let encoded = vec![*code];
                    let (unicode, unicode_provenance) = self.unicode_for(&encoded, || {
                        let candidate = font.encoding.unicode[usize::from(*code)];
                        let provenance = if font.encoding.embedded_type1[usize::from(*code)] {
                            UnicodeProvenance::EmbeddedType1Encoding
                        } else if font.encoding.differences_agl[usize::from(*code)] {
                            UnicodeProvenance::DifferencesAgl
                        } else {
                            UnicodeProvenance::SimpleEncoding
                        };
                        match &font.embedded_unicode {
                            EmbeddedUnicode::Absent => candidate,
                            EmbeddedUnicode::Valid(characters) => {
                                candidate.filter(|character| characters.contains(character))
                            }
                            EmbeddedUnicode::Invalid => None,
                        }
                        .map(|character| (character, provenance))
                    });
                    let width = font.widths.as_ref().and_then(|widths| {
                        usize::try_from(i64::from(*code) - font.first_char)
                            .ok()
                            .and_then(|index| widths.get(index))
                            .copied()
                    });
                    DecodedGlyph {
                        unicode,
                        unicode_provenance,
                        code: u32::from(*code),
                        encoded,
                        advance_em: width.or(font.missing_width).unwrap_or(0.0) / 1000.0,
                        font_supported: self.supported,
                    }
                })
                .collect(),
            FontKind::Composite(font) => {
                let segments = font.segments(bytes, &self.unicode);
                segments
                    .into_iter()
                    .map(|(encoded, cid)| {
                        let fallback = || {
                            let cid = cid?;
                            let gid = font.cid_to_gid.gid(cid)?;
                            font.gid_to_unicode
                                .get(&gid)
                                .copied()
                                .map(|character| (character, UnicodeProvenance::EmbeddedFontCmap))
                        };
                        let (unicode, unicode_provenance) = self.unicode_for(&encoded, fallback);
                        DecodedGlyph {
                            unicode,
                            unicode_provenance,
                            code: cid.unwrap_or_else(|| bytes_to_u32(&encoded).unwrap_or(0)),
                            advance_em: cid
                                .map(|value| font.widths.width(value) / 1000.0)
                                .unwrap_or(0.0),
                            encoded,
                            font_supported: self.supported,
                        }
                    })
                    .collect()
            }
            FontKind::Type3(font) => bytes
                .iter()
                .map(|code| {
                    let encoded = vec![*code];
                    let name = font.encoding.glyph_names[usize::from(*code)].as_ref();
                    let glyph = name.and_then(|name| font.glyphs.get(name));
                    let (unicode, unicode_provenance) = self.unicode_for(&encoded, || {
                        font.encoding.unicode[usize::from(*code)].map(|character| {
                            let provenance = if font.encoding.differences_agl[usize::from(*code)] {
                                UnicodeProvenance::DifferencesAgl
                            } else {
                                UnicodeProvenance::SimpleEncoding
                            };
                            (character, provenance)
                        })
                    });
                    DecodedGlyph {
                        unicode,
                        unicode_provenance,
                        code: u32::from(*code),
                        encoded,
                        advance_em: glyph.map_or(0.0, |glyph| glyph.advance_em),
                        font_supported: self.supported && glyph.is_some(),
                    }
                })
                .collect(),
            FontKind::Unknown => bytes
                .iter()
                .map(|code| DecodedGlyph {
                    unicode: Vec::new(),
                    unicode_provenance: UnicodeProvenance::Unresolved,
                    code: u32::from(*code),
                    encoded: vec![*code],
                    advance_em: 0.0,
                    font_supported: false,
                })
                .collect(),
        }
    }

    fn unicode_for(
        &self,
        encoded: &[u8],
        fallback: impl FnOnce() -> Option<(char, UnicodeProvenance)>,
    ) -> (Vec<char>, UnicodeProvenance) {
        match &self.unicode {
            UnicodeSource::Absent => fallback().map_or_else(
                || (Vec::new(), UnicodeProvenance::Unresolved),
                |(character, provenance)| (vec![character], provenance),
            ),
            UnicodeSource::Valid(map) => map.chars.get(encoded).map_or_else(
                || (Vec::new(), UnicodeProvenance::Unresolved),
                |characters| {
                    if characters.iter().copied().any(is_unicode_noncharacter) {
                        (Vec::new(), UnicodeProvenance::Unresolved)
                    } else {
                        (characters.clone(), UnicodeProvenance::ToUnicode)
                    }
                },
            ),
            UnicodeSource::Invalid => (Vec::new(), UnicodeProvenance::Unresolved),
        }
    }
}

fn is_unicode_noncharacter(character: char) -> bool {
    let value = u32::from(character);
    (0xFDD0..=0xFDEF).contains(&value) || value & 0xFFFF >= 0xFFFE
}

fn dangling_font_reference(
    document: &Document,
    resources: &Dictionary,
    name: &[u8],
) -> Option<ObjectId> {
    let fonts = resources
        .get_deref(b"Font", document)
        .ok()?
        .as_dict()
        .ok()?;
    let Object::Reference(object_id) = fonts.get(name).ok()? else {
        return None;
    };
    document
        .get_object(*object_id)
        .is_err()
        .then_some(*object_id)
}

impl CompositeFont {
    fn segments(&self, bytes: &[u8], unicode: &UnicodeSource) -> Vec<(Vec<u8>, Option<u32>)> {
        match &self.encoding {
            CompositeEncoding::Identity => bytes
                .chunks(2)
                .map(|chunk| {
                    let encoded = chunk.to_vec();
                    let cid = (chunk.len() == 2).then(|| bytes_to_u32(chunk)).flatten();
                    (encoded, cid)
                })
                .collect(),
            CompositeEncoding::Embedded(map) => map
                .segment(bytes)
                .into_iter()
                .map(|encoded| {
                    let cid = map.lookup(&encoded);
                    (encoded, cid)
                })
                .collect(),
            CompositeEncoding::Unsupported => match unicode {
                UnicodeSource::Valid(map) => map
                    .segment(bytes)
                    .into_iter()
                    .map(|encoded| (encoded, None))
                    .collect(),
                UnicodeSource::Absent | UnicodeSource::Invalid => {
                    bytes.iter().map(|byte| (vec![*byte], None)).collect()
                }
            },
        }
    }
}

impl CodeMap {
    fn lookup(&self, code: &[u8]) -> Option<u32> {
        self.chars.get(code).copied().or_else(|| {
            let value = bytes_to_u32(code)?;
            self.ranges.iter().find_map(|range| {
                if range.low.len() != code.len() {
                    return None;
                }
                let low = bytes_to_u32(&range.low)?;
                let high = bytes_to_u32(&range.high)?;
                (value >= low && value <= high).then_some(range.first + value - low)
            })
        })
    }

    fn segment(&self, bytes: &[u8]) -> Vec<Vec<u8>> {
        segment_codes(bytes, &self.codespaces)
    }
}

impl UnicodeMap {
    fn segment(&self, bytes: &[u8]) -> Vec<Vec<u8>> {
        segment_codes(bytes, &self.codespaces)
    }
}

impl CidWidths {
    fn width(&self, cid: u32) -> f64 {
        self.explicit.get(&cid).copied().unwrap_or(self.default)
    }
}

impl CidToGid {
    fn gid(&self, cid: u32) -> Option<u16> {
        match self {
            Self::Identity => u16::try_from(cid).ok(),
            Self::Map(bytes) => {
                let offset = usize::try_from(cid).ok()?.checked_mul(2)?;
                Some(u16::from_be_bytes([
                    *bytes.get(offset)?,
                    *bytes.get(offset + 1)?,
                ]))
            }
            Self::Invalid => None,
        }
    }
}

fn read_to_unicode(document: &Document, font: &Dictionary) -> UnicodeSource {
    let Ok(object) = font.get_deref(b"ToUnicode", document) else {
        return UnicodeSource::Absent;
    };
    let Ok(stream) = object.as_stream() else {
        return UnicodeSource::Invalid;
    };
    let Ok(bytes) = pdf_stream::decode(document, stream, MAX_STREAM_BYTES) else {
        return UnicodeSource::Invalid;
    };
    parse_unicode_cmap(&bytes)
        .map(UnicodeSource::Valid)
        .unwrap_or(UnicodeSource::Invalid)
}

fn read_simple_encoding(
    document: &Document,
    font: &Dictionary,
    true_type: bool,
) -> FontResult<SimpleEncoding> {
    let mut encoding = base_encoding(if true_type {
        b"WinAnsiEncoding"
    } else {
        b"StandardEncoding"
    });
    let Ok(object) = font.get_deref(b"Encoding", document) else {
        return Ok(encoding);
    };
    match object {
        Object::Name(name) => Ok(base_encoding(name)),
        Object::Dictionary(dictionary) => {
            if let Ok(name) = dictionary.get(b"BaseEncoding").and_then(Object::as_name) {
                encoding = base_encoding(name);
            }
            if let Ok(differences) = dictionary.get(b"Differences").and_then(Object::as_array) {
                let mut code = None;
                for item in differences {
                    match item {
                        Object::Integer(value) => code = u8::try_from(*value).ok(),
                        Object::Name(name) => {
                            let current = code.ok_or(FontFailure)?;
                            let existing = glyph_name_to_unicode(name);
                            let recovered = existing.or_else(|| {
                                mimus_quality_contract::differences_agl_single_scalar(name)
                            });
                            encoding.unicode[usize::from(current)] = recovered;
                            encoding.differences_agl[usize::from(current)] =
                                existing.is_none() && recovered.is_some();
                            encoding.glyph_names[usize::from(current)] = Some(name.clone());
                            code = current.checked_add(1);
                        }
                        _ => return Err(FontFailure),
                    }
                }
            }
            Ok(encoding)
        }
        _ => Err(FontFailure),
    }
}

fn normalize_descriptor_descent(value: f64) -> f64 {
    if value > 0.0 { -value } else { value }
}

/// Parse only the bounded cleartext encoding declarations in a Type1 program.
/// The encrypted CharStrings section starts at `eexec` and is never scanned.
fn read_embedded_type1_encoding(
    document: &Document,
    descriptor: &Dictionary,
) -> Option<FontResult<SimpleEncoding>> {
    let object = descriptor.get_deref(b"FontFile", document).ok()?;
    let stream = match object.as_stream() {
        Ok(stream) => stream,
        Err(_) => return Some(Err(FontFailure)),
    };
    let bytes = match pdf_stream::decode(document, stream, MAX_STREAM_BYTES) {
        Ok(bytes) => bytes,
        Err(_) => return Some(Err(FontFailure)),
    };
    Some(parse_type1_cleartext_encoding(&bytes))
}

fn parse_type1_cleartext_encoding(bytes: &[u8]) -> FontResult<SimpleEncoding> {
    let cleartext_end = bytes
        .windows(b"eexec".len())
        .position(|window| window == b"eexec")
        .ok_or(FontFailure)?;
    let tokens = bytes[..cleartext_end]
        .split(u8::is_ascii_whitespace)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let mut encoding = SimpleEncoding {
        unicode: vec![None; 256],
        glyph_names: vec![None; 256],
        differences_agl: vec![false; 256],
        embedded_type1: vec![false; 256],
    };
    let mut declarations = 0usize;
    for declaration in tokens.windows(4) {
        if declaration[0] != b"dup" || declaration[3] != b"put" {
            continue;
        }
        let code = std::str::from_utf8(declaration[1])
            .ok()
            .and_then(|value| value.parse::<u8>().ok());
        let name = declaration[2].strip_prefix(b"/");
        let (Some(code), Some(name)) = (code, name) else {
            continue;
        };
        let unicode = glyph_name_to_unicode(name)
            .or_else(|| mimus_quality_contract::differences_agl_single_scalar(name));
        encoding.unicode[usize::from(code)] = unicode;
        encoding.glyph_names[usize::from(code)] = Some(name.to_vec());
        encoding.embedded_type1[usize::from(code)] = true;
        declarations += 1;
    }
    (declarations > 0).then_some(encoding).ok_or(FontFailure)
}

fn base_encoding(name: &[u8]) -> SimpleEncoding {
    let mut encoding = SimpleEncoding {
        unicode: vec![None; 256],
        glyph_names: vec![None; 256],
        differences_agl: vec![false; 256],
        embedded_type1: vec![false; 256],
    };
    for code in 0_u8..=u8::MAX {
        let unicode = match name {
            b"WinAnsiEncoding" => super::decode_win_ansi(code),
            b"MacRomanEncoding" => code
                .is_ascii()
                .then(|| char::from(code))
                .filter(|character| !character.is_control()),
            b"StandardEncoding" => standard_encoding(code),
            _ => None,
        };
        encoding.unicode[usize::from(code)] = unicode;
        encoding.glyph_names[usize::from(code)] = unicode.map(unicode_glyph_name);
    }
    encoding
}

fn standard_encoding(code: u8) -> Option<char> {
    (code.is_ascii())
        .then(|| char::from(code))
        .filter(|character| !character.is_control())
}

fn unicode_glyph_name(character: char) -> Vec<u8> {
    match character {
        ' ' => b"space".to_vec(),
        value if value.is_ascii_alphanumeric() => vec![value as u8],
        value => format!("uni{:04X}", u32::from(value)).into_bytes(),
    }
}

fn glyph_name_to_unicode(name: &[u8]) -> Option<char> {
    if name.len() == 1 && name[0].is_ascii_alphabetic() {
        return Some(char::from(name[0]));
    }
    match name {
        b"space" => Some(' '),
        b"zero" => Some('0'),
        b"one" => Some('1'),
        b"two" => Some('2'),
        b"three" => Some('3'),
        b"four" => Some('4'),
        b"five" => Some('5'),
        b"six" => Some('6'),
        b"seven" => Some('7'),
        b"eight" => Some('8'),
        b"nine" => Some('9'),
        b"period" => Some('.'),
        b"comma" => Some(','),
        b"hyphen" => Some('-'),
        b"parenleft" => Some('('),
        b"parenright" => Some(')'),
        _ => parse_unicode_glyph_name(name),
    }
}

fn parse_unicode_glyph_name(name: &[u8]) -> Option<char> {
    let digits = name
        .strip_prefix(b"uni")
        .or_else(|| name.strip_prefix(b"u"))?;
    if !(4..=6).contains(&digits.len()) || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let value = std::str::from_utf8(digits).ok()?;
    char::from_u32(u32::from_str_radix(value, 16).ok()?)
}

fn embedded_true_type(document: &Document, descriptor: &Dictionary) -> Option<Vec<u8>> {
    let stream = descriptor
        .get_deref(b"FontFile2", document)
        .ok()?
        .as_stream()
        .ok()?;
    pdf_stream::decode(document, stream, MAX_STREAM_BYTES).ok()
}

fn read_embedded_unicode(document: &Document, descriptor: &Dictionary) -> EmbeddedUnicode {
    let Ok(object) = descriptor.get_deref(b"FontFile2", document) else {
        return if descriptor.has(b"FontFile2") {
            EmbeddedUnicode::Invalid
        } else {
            EmbeddedUnicode::Absent
        };
    };
    let Ok(stream) = object.as_stream() else {
        return EmbeddedUnicode::Invalid;
    };
    let Ok(bytes) = pdf_stream::decode(document, stream, MAX_STREAM_BYTES) else {
        return EmbeddedUnicode::Invalid;
    };
    unicode_cmap_characters(&bytes)
        .map(EmbeddedUnicode::Valid)
        .unwrap_or(EmbeddedUnicode::Invalid)
}

fn unicode_cmap_characters(bytes: &[u8]) -> FontResult<BTreeSet<char>> {
    let face = ttf_parser::Face::parse(bytes, 0).map_err(|_| FontFailure)?;
    let cmap = face.tables().cmap.ok_or(FontFailure)?;
    let mut characters = BTreeSet::new();
    for subtable in cmap
        .subtables
        .into_iter()
        .filter(|table| table.is_unicode())
    {
        subtable.codepoints(|codepoint| {
            if let Some(character) = char::from_u32(codepoint)
                && subtable.glyph_index(codepoint).is_some()
            {
                characters.insert(character);
            }
        });
    }
    (!characters.is_empty())
        .then_some(characters)
        .ok_or(FontFailure)
}

fn reverse_unicode_cmap(bytes: &[u8]) -> FontResult<BTreeMap<u16, char>> {
    let face = ttf_parser::Face::parse(bytes, 0).map_err(|_| FontFailure)?;
    let cmap = face.tables().cmap.ok_or(FontFailure)?;
    let mut reverse = BTreeMap::new();
    for subtable in cmap
        .subtables
        .into_iter()
        .filter(|table| table.is_unicode())
    {
        subtable.codepoints(|codepoint| {
            if let (Some(character), Some(glyph)) =
                (char::from_u32(codepoint), subtable.glyph_index(codepoint))
            {
                reverse.entry(glyph.0).or_insert(character);
            }
        });
    }
    (!reverse.is_empty()).then_some(reverse).ok_or(FontFailure)
}

fn read_cid_to_gid(document: &Document, descendant: &Dictionary) -> Option<CidToGid> {
    let object = descendant.get_deref(b"CIDToGIDMap", document).ok();
    match object {
        None => Some(CidToGid::Identity),
        Some(Object::Name(name)) if name == b"Identity" => Some(CidToGid::Identity),
        Some(Object::Name(_)) => None,
        Some(Object::Stream(stream)) => pdf_stream::decode(document, stream, MAX_STREAM_BYTES)
            .ok()
            .filter(|bytes| bytes.len() % 2 == 0)
            .map(CidToGid::Map),
        _ => None,
    }
}

fn read_cid_widths(document: &Document, descendant: &Dictionary) -> FontResult<CidWidths> {
    let default = descendant
        .get(b"DW")
        .ok()
        .and_then(object_number)
        .unwrap_or(1000.0);
    let mut explicit = BTreeMap::new();
    let Some(items) = descendant
        .get_deref(b"W", document)
        .ok()
        .and_then(|object| object.as_array().ok())
    else {
        return Ok(CidWidths { default, explicit });
    };
    let mut index = 0;
    while index < items.len() {
        let first = items[index].as_i64().map_err(|_| FontFailure)?;
        let first = u32::try_from(first).map_err(|_| FontFailure)?;
        index += 1;
        match items.get(index) {
            Some(Object::Array(widths)) => {
                for (offset, width) in widths.iter().enumerate() {
                    explicit.insert(
                        first + u32::try_from(offset).map_err(|_| FontFailure)?,
                        object_number(width).ok_or(FontFailure)?,
                    );
                }
                index += 1;
            }
            Some(last) => {
                let last = u32::try_from(last.as_i64().map_err(|_| FontFailure)?)
                    .map_err(|_| FontFailure)?;
                let width = items
                    .get(index + 1)
                    .and_then(object_number)
                    .ok_or(FontFailure)?;
                if last < first {
                    return Err(FontFailure);
                }
                for cid in first..=last {
                    explicit.insert(cid, width);
                }
                index += 2;
            }
            None => return Err(FontFailure),
        }
    }
    Ok(CidWidths { default, explicit })
}

fn type3_advance(bytes: &[u8]) -> Option<(f64, f64)> {
    let words = bytes
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|word| !word.is_empty())
        .take(8)
        .collect::<Vec<_>>();
    for (index, word) in words.iter().enumerate() {
        let operand_start = match *word {
            b"d0" if index >= 2 => index - 2,
            b"d1" if index >= 6 => index - 6,
            _ => continue,
        };
        {
            let wx = std::str::from_utf8(words[operand_start])
                .ok()?
                .parse::<f64>()
                .ok()?;
            let wy = std::str::from_utf8(words[operand_start + 1])
                .ok()?
                .parse::<f64>()
                .ok()?;
            return (wx.is_finite() && wy.is_finite()).then_some((wx, wy));
        }
    }
    None
}

fn parse_encoding_cmap(bytes: &[u8]) -> FontResult<CodeMap> {
    let tokens = cmap_tokens(bytes)?;
    let mut map = CodeMap::default();
    parse_sections(&tokens, |kind, entries| match kind {
        Section::CodeSpace => {
            for entry in entries.as_chunks::<2>().0 {
                let (CMapToken::Bytes(low), CMapToken::Bytes(high)) = (&entry[0], &entry[1]) else {
                    return Err(FontFailure);
                };
                validate_codespace(low, high)?;
                map.codespaces.push(CodeSpace {
                    low: low.clone(),
                    high: high.clone(),
                });
            }
            Ok(())
        }
        Section::CidChar => {
            for entry in entries.as_chunks::<2>().0 {
                let (CMapToken::Bytes(code), CMapToken::Number(cid)) = (&entry[0], &entry[1])
                else {
                    return Err(FontFailure);
                };
                map.chars
                    .insert(code.clone(), u32::try_from(*cid).map_err(|_| FontFailure)?);
            }
            Ok(())
        }
        Section::CidRange => {
            for entry in entries.as_chunks::<3>().0 {
                let (CMapToken::Bytes(low), CMapToken::Bytes(high), CMapToken::Number(first)) =
                    (&entry[0], &entry[1], &entry[2])
                else {
                    return Err(FontFailure);
                };
                validate_codespace(low, high)?;
                map.ranges.push(CodeRange {
                    low: low.clone(),
                    high: high.clone(),
                    first: u32::try_from(*first).map_err(|_| FontFailure)?,
                });
            }
            Ok(())
        }
        Section::BfChar | Section::BfRange => Ok(()),
    })?;
    if map.codespaces.is_empty() {
        return Err(FontFailure);
    }
    Ok(map)
}

fn parse_unicode_cmap(bytes: &[u8]) -> FontResult<UnicodeMap> {
    let tokens = cmap_tokens(bytes)?;
    let mut map = UnicodeMap::default();
    parse_sections(&tokens, |kind, entries| match kind {
        Section::CodeSpace => {
            for entry in entries.as_chunks::<2>().0 {
                let (CMapToken::Bytes(low), CMapToken::Bytes(high)) = (&entry[0], &entry[1]) else {
                    return Err(FontFailure);
                };
                validate_codespace(low, high)?;
                map.codespaces.push(CodeSpace {
                    low: low.clone(),
                    high: high.clone(),
                });
            }
            Ok(())
        }
        Section::BfChar => {
            for entry in entries.as_chunks::<2>().0 {
                let (CMapToken::Bytes(code), CMapToken::Bytes(unicode)) = (&entry[0], &entry[1])
                else {
                    return Err(FontFailure);
                };
                map.chars.insert(code.clone(), decode_utf16be(unicode)?);
            }
            Ok(())
        }
        Section::BfRange => parse_bf_ranges(entries, &mut map.chars),
        Section::CidChar | Section::CidRange => Ok(()),
    })?;
    if map.codespaces.is_empty() || map.chars.is_empty() {
        return Err(FontFailure);
    }
    Ok(map)
}

#[derive(Debug, Clone)]
enum CMapToken {
    Bytes(Vec<u8>),
    Number(i64),
    Word(Vec<u8>),
    ArrayStart,
    ArrayEnd,
}

#[derive(Debug, Clone, Copy)]
enum Section {
    CodeSpace,
    CidChar,
    CidRange,
    BfChar,
    BfRange,
}

fn cmap_tokens(bytes: &[u8]) -> FontResult<Vec<CMapToken>> {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        if bytes[cursor] == b'%' {
            while cursor < bytes.len() && !matches!(bytes[cursor], b'\r' | b'\n') {
                cursor += 1;
            }
            continue;
        }
        match bytes[cursor] {
            b'<' if bytes.get(cursor + 1) == Some(&b'<') => {
                cursor += 2;
                tokens.push(CMapToken::Word(b"<<".to_vec()));
            }
            b'<' if bytes.get(cursor + 1) != Some(&b'<') => {
                cursor += 1;
                let mut nibbles = Vec::new();
                while cursor < bytes.len() && bytes[cursor] != b'>' {
                    if !bytes[cursor].is_ascii_whitespace() {
                        nibbles.push(hex(bytes[cursor]).ok_or(FontFailure)?);
                    }
                    cursor += 1;
                }
                if bytes.get(cursor) != Some(&b'>') {
                    return Err(FontFailure);
                }
                cursor += 1;
                if nibbles.len() % 2 == 1 {
                    nibbles.push(0);
                }
                tokens.push(CMapToken::Bytes(
                    nibbles
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| pair[0] << 4 | pair[1])
                        .collect(),
                ));
            }
            b'[' => {
                cursor += 1;
                tokens.push(CMapToken::ArrayStart);
            }
            b']' => {
                cursor += 1;
                tokens.push(CMapToken::ArrayEnd);
            }
            b'>' if bytes.get(cursor + 1) == Some(&b'>') => {
                cursor += 2;
                tokens.push(CMapToken::Word(b">>".to_vec()));
            }
            _ => {
                let start = cursor;
                while cursor < bytes.len()
                    && !bytes[cursor].is_ascii_whitespace()
                    && !matches!(bytes[cursor], b'<' | b'>' | b'[' | b']')
                {
                    cursor += 1;
                }
                if start == cursor {
                    return Err(FontFailure);
                }
                let word = &bytes[start..cursor];
                if let Ok(text) = std::str::from_utf8(word)
                    && let Ok(value) = text.parse::<i64>()
                {
                    tokens.push(CMapToken::Number(value));
                } else {
                    tokens.push(CMapToken::Word(word.to_vec()));
                }
            }
        }
        if tokens.len() > 1_000_000 {
            return Err(FontFailure);
        }
    }
    Ok(tokens)
}

fn parse_sections(
    tokens: &[CMapToken],
    mut consume: impl FnMut(Section, &[CMapToken]) -> FontResult<()>,
) -> FontResult<()> {
    let mut index = 0;
    while index + 1 < tokens.len() {
        let (CMapToken::Number(count), CMapToken::Word(word)) =
            (&tokens[index], &tokens[index + 1])
        else {
            index += 1;
            continue;
        };
        let Some((section, arity, end)) = section(word) else {
            index += 1;
            continue;
        };
        let count = usize::try_from(*count).map_err(|_| FontFailure)?;
        let start = index + 2;
        let finish = if matches!(section, Section::BfRange) {
            find_section_end(tokens, start, end)?
        } else {
            start
                .checked_add(count.checked_mul(arity).ok_or(FontFailure)?)
                .ok_or(FontFailure)?
        };
        if !matches!(tokens.get(finish), Some(CMapToken::Word(value)) if value == end) {
            return Err(FontFailure);
        }
        if matches!(section, Section::BfRange) {
            validate_bfrange_count(&tokens[start..finish], count)?;
        } else if finish - start != count * arity {
            return Err(FontFailure);
        }
        consume(section, &tokens[start..finish])?;
        index = finish + 1;
    }
    Ok(())
}

fn section(word: &[u8]) -> Option<(Section, usize, &'static [u8])> {
    match word {
        b"begincodespacerange" => Some((Section::CodeSpace, 2, b"endcodespacerange")),
        b"begincidchar" => Some((Section::CidChar, 2, b"endcidchar")),
        b"begincidrange" => Some((Section::CidRange, 3, b"endcidrange")),
        b"beginbfchar" => Some((Section::BfChar, 2, b"endbfchar")),
        b"beginbfrange" => Some((Section::BfRange, 0, b"endbfrange")),
        _ => None,
    }
}

fn find_section_end(tokens: &[CMapToken], start: usize, end: &[u8]) -> FontResult<usize> {
    tokens[start..]
        .iter()
        .position(|token| matches!(token, CMapToken::Word(word) if word == end))
        .map(|offset| start + offset)
        .ok_or(FontFailure)
}

fn validate_bfrange_count(tokens: &[CMapToken], expected: usize) -> FontResult<()> {
    let mut index = 0;
    let mut actual = 0;
    while index < tokens.len() {
        if !matches!(tokens.get(index), Some(CMapToken::Bytes(_)))
            || !matches!(tokens.get(index + 1), Some(CMapToken::Bytes(_)))
        {
            return Err(FontFailure);
        }
        index += 2;
        match tokens.get(index) {
            Some(CMapToken::Bytes(_)) => index += 1,
            Some(CMapToken::ArrayStart) => {
                index += 1;
                while matches!(tokens.get(index), Some(CMapToken::Bytes(_))) {
                    index += 1;
                }
                if !matches!(tokens.get(index), Some(CMapToken::ArrayEnd)) {
                    return Err(FontFailure);
                }
                index += 1;
            }
            _ => return Err(FontFailure),
        }
        actual += 1;
    }
    (actual == expected).then_some(()).ok_or(FontFailure)
}

fn parse_bf_ranges(
    tokens: &[CMapToken],
    output: &mut BTreeMap<Vec<u8>, Vec<char>>,
) -> FontResult<()> {
    let mut index = 0;
    while index < tokens.len() {
        let (Some(CMapToken::Bytes(low)), Some(CMapToken::Bytes(high))) =
            (tokens.get(index), tokens.get(index + 1))
        else {
            return Err(FontFailure);
        };
        validate_codespace(low, high)?;
        let low_value = bytes_to_u32(low).ok_or(FontFailure)?;
        let high_value = bytes_to_u32(high).ok_or(FontFailure)?;
        index += 2;
        match tokens.get(index) {
            Some(CMapToken::Bytes(first)) => {
                let first = bytes_to_u32(first).ok_or(FontFailure)?;
                for offset in 0..=high_value - low_value {
                    output.insert(
                        u32_to_bytes(low_value + offset, low.len()),
                        vec![char::from_u32(first + offset).ok_or(FontFailure)?],
                    );
                }
                index += 1;
            }
            Some(CMapToken::ArrayStart) => {
                index += 1;
                for offset in 0..=high_value - low_value {
                    let Some(CMapToken::Bytes(unicode)) = tokens.get(index) else {
                        return Err(FontFailure);
                    };
                    output.insert(
                        u32_to_bytes(low_value + offset, low.len()),
                        decode_utf16be(unicode)?,
                    );
                    index += 1;
                }
                if !matches!(tokens.get(index), Some(CMapToken::ArrayEnd)) {
                    return Err(FontFailure);
                }
                index += 1;
            }
            _ => return Err(FontFailure),
        }
    }
    Ok(())
}

fn validate_codespace(low: &[u8], high: &[u8]) -> FontResult<()> {
    if low.is_empty()
        || low.len() != high.len()
        || low.len() > 4
        || bytes_to_u32(low) > bytes_to_u32(high)
    {
        return Err(FontFailure);
    }
    Ok(())
}

fn segment_codes(bytes: &[u8], spaces: &[CodeSpace]) -> Vec<Vec<u8>> {
    let mut output = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let mut lengths = spaces
            .iter()
            .map(|space| space.low.len())
            .collect::<Vec<_>>();
        lengths.sort_unstable();
        lengths.dedup();
        let matched = lengths.into_iter().find(|length| {
            let Some(code) = bytes.get(cursor..cursor + *length) else {
                return false;
            };
            spaces.iter().any(|space| {
                space.low.len() == *length
                    && code >= space.low.as_slice()
                    && code <= space.high.as_slice()
            })
        });
        let length = matched.unwrap_or(1);
        let end = (cursor + length).min(bytes.len());
        output.push(bytes[cursor..end].to_vec());
        cursor = end;
    }
    output
}

fn decode_utf16be(bytes: &[u8]) -> FontResult<Vec<char>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(2) {
        return Err(FontFailure);
    }
    let words = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let mut chars = char::decode_utf16(words)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| FontFailure)?;
    if chars.first() == Some(&'\u{feff}') {
        chars.remove(0);
    }
    if chars.is_empty() {
        return Err(FontFailure);
    }
    Ok(chars)
}

fn bytes_to_u32(bytes: &[u8]) -> Option<u32> {
    (bytes.len() <= 4).then(|| {
        bytes
            .iter()
            .fold(0_u32, |value, byte| value << 8 | u32::from(*byte))
    })
}

fn u32_to_bytes(value: u32, length: usize) -> Vec<u8> {
    value.to_be_bytes()[4 - length..].to_vec()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn dereference_dictionary<'a>(
    document: &'a Document,
    object: &'a Object,
) -> FontResult<(&'a Dictionary, Option<ObjectId>)> {
    match object {
        Object::Reference(object_id) => document
            .get_object(*object_id)
            .and_then(Object::as_dict)
            .map(|dictionary| (dictionary, Some(*object_id)))
            .map_err(|_| FontFailure),
        Object::Dictionary(dictionary) => Ok((dictionary, None)),
        _ => Err(FontFailure),
    }
}

fn font_dictionary_is_bold(font: &Dictionary, descriptor: Option<&Dictionary>) -> bool {
    font.get(b"BaseFont")
        .and_then(Object::as_name)
        .is_ok_and(font_name_is_bold)
        || descriptor.is_some_and(|descriptor| {
            descriptor
                .get(b"FontName")
                .and_then(Object::as_name)
                .is_ok_and(font_name_is_bold)
        })
}

fn font_name_is_bold(name: &[u8]) -> bool {
    name.windows(b"bold".len())
        .any(|part| part.eq_ignore_ascii_case(b"bold"))
}

fn resource_name(name: &[u8]) -> String {
    let mut output = String::new();
    for byte in name {
        if byte.is_ascii_graphic() && !matches!(byte, b'#' | b'/' | b'%' | b'(' | b')') {
            output.push(char::from(*byte));
        } else {
            output.push_str(&format!("#{byte:02X}"));
        }
    }
    output
}

fn is_standard_14_name(name: &[u8]) -> bool {
    let base = name.rsplit(|byte| *byte == b'+').next().unwrap_or(name);
    matches!(
        base,
        b"Times-Roman"
            | b"Times-Bold"
            | b"Times-Italic"
            | b"Times-BoldItalic"
            | b"Helvetica"
            | b"Helvetica-Bold"
            | b"Helvetica-Oblique"
            | b"Helvetica-BoldOblique"
            | b"Courier"
            | b"Courier-Bold"
            | b"Courier-Oblique"
            | b"Courier-BoldOblique"
            | b"Symbol"
            | b"ZapfDingbats"
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn font_streams_use_the_bounded_decoder_for_indirect_ascii_hex_filters() {
        let mut document = Document::new();
        let filter = document.add_object(Object::Name(b"ASCIIHexDecode".to_vec()));
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange \
                     1 beginbfchar <41> <005A> endbfchar";
        let mut encoded = cmap
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>()
            .into_bytes();
        encoded.push(b'>');
        let stream = document.add_object(lopdf::Stream::new(
            lopdf::dictionary! { "Filter" => filter },
            encoded,
        ));
        let font = lopdf::dictionary! { "ToUnicode" => stream };

        let UnicodeSource::Valid(map) = read_to_unicode(&document, &font) else {
            panic!("indirect ASCIIHexDecode ToUnicode stream was rejected");
        };
        assert_eq!(map.chars.get(b"A".as_slice()), Some(&vec!['Z']));
    }

    #[test]
    fn unknown_simple_encoding_names_do_not_fall_back_to_standard_encoding() {
        let encoding = base_encoding(b"VendorEncoding");

        assert!(encoding.unicode.iter().all(Option::is_none));
        assert!(encoding.glyph_names.iter().all(Option::is_none));
    }

    #[test]
    fn implicit_standard_encoding_never_claims_differences_agl_provenance() {
        let encoding = base_encoding(b"StandardEncoding");

        assert_eq!(encoding.unicode[usize::from(b'A')], Some('A'));
        assert!(!encoding.differences_agl[usize::from(b'A')]);
    }

    #[test]
    fn mixed_codespaces_segment_without_guessing_width() {
        let map = parse_encoding_cmap(
            b"2 begincodespacerange <00> <80> <8140> <FEFE> endcodespacerange \
              3 begincidchar <07> 7 <06> 6 <8140> 7 endcidchar",
        )
        .unwrap();
        assert_eq!(
            map.segment(&[0x07, 0x06, 0x81, 0x40]),
            [vec![7], vec![6], vec![0x81, 0x40]]
        );
        assert_eq!(map.lookup(&[0x81, 0x40]), Some(7));
    }

    #[test]
    fn malformed_bfrange_count_invalidates_the_whole_map() {
        assert!(
            parse_unicode_cmap(
                b"1 begincodespacerange <00> <FF> endcodespacerange \
                  2 beginbfrange <41> <41> <0041> endbfrange"
            )
            .is_err()
        );
    }

    #[test]
    fn to_unicode_preserves_multi_scalar_mappings() {
        let map = parse_unicode_cmap(
            b"1 begincodespacerange <00> <FF> endcodespacerange \
              1 beginbfchar <01> <00660069> endbfchar",
        )
        .unwrap();

        assert_eq!(map.chars.get(&[1][..]), Some(&vec!['f', 'i']));
    }

    #[test]
    fn all_unicode_noncharacters_are_rejected() {
        let mut noncharacters = (0xFDD0..=0xFDEF)
            .map(|value| char::from_u32(value).unwrap())
            .collect::<Vec<_>>();
        for plane in 0..=16 {
            noncharacters.push(char::from_u32((plane << 16) | 0xFFFE).unwrap());
            noncharacters.push(char::from_u32((plane << 16) | 0xFFFF).unwrap());
        }

        assert_eq!(noncharacters.len(), 66);
        assert!(noncharacters.into_iter().all(is_unicode_noncharacter));
        assert!(!is_unicode_noncharacter('\u{FDCF}'));
        assert!(!is_unicode_noncharacter('\u{FDF0}'));
        assert!(!is_unicode_noncharacter('\u{10FFFD}'));
    }

    #[test]
    fn to_unicode_noncharacters_become_unresolved_without_falling_back() {
        let font = ResolvedFont {
            reference: FontRef {
                resource_name: "F1".to_owned(),
                object_number: 1,
                generation: 0,
            },
            is_bold: false,
            ascent_em: 0.8,
            descent_em: -0.2,
            normalized_descriptor_descent: false,
            engine_mismatch_tolerated: false,
            supported: true,
            unicode: UnicodeSource::Valid(UnicodeMap {
                codespaces: Vec::new(),
                chars: BTreeMap::from([(b"A".to_vec(), vec!['\u{FFFF}'])]),
            }),
            kind: FontKind::Unknown,
        };
        let fallback_called = Cell::new(false);

        let decoded = font.unicode_for(b"A", || {
            fallback_called.set(true);
            Some(('Z', UnicodeProvenance::EmbeddedFontCmap))
        });

        assert_eq!(decoded, (Vec::new(), UnicodeProvenance::Unresolved));
        assert!(!fallback_called.get());
    }

    #[test]
    fn to_unicode_unmapped_codes_never_fall_back_to_differences() {
        let font = ResolvedFont {
            reference: FontRef {
                resource_name: "F1".to_owned(),
                object_number: 1,
                generation: 0,
            },
            is_bold: false,
            ascent_em: 0.8,
            descent_em: -0.2,
            normalized_descriptor_descent: false,
            engine_mismatch_tolerated: false,
            supported: true,
            unicode: UnicodeSource::Valid(UnicodeMap {
                codespaces: Vec::new(),
                chars: BTreeMap::from([(b"B".to_vec(), vec!['B'])]),
            }),
            kind: FontKind::Unknown,
        };
        let fallback_called = Cell::new(false);

        let decoded = font.unicode_for(b"A", || {
            fallback_called.set(true);
            Some(('Á', UnicodeProvenance::DifferencesAgl))
        });

        assert_eq!(decoded, (Vec::new(), UnicodeProvenance::Unresolved));
        assert!(!fallback_called.get());
    }

    #[test]
    fn differences_reject_unknown_names_instead_of_inventing_unicode() {
        assert_eq!(glyph_name_to_unicode(b"M"), Some('M'));
        assert_eq!(glyph_name_to_unicode(b"uni4E2D"), Some('\u{4e2d}'));
        assert_eq!(glyph_name_to_unicode(b"0"), None);
        assert_eq!(glyph_name_to_unicode(b"g123"), None);
    }

    #[test]
    fn positive_descriptor_descent_is_normalized_below_the_baseline() {
        assert_eq!(normalize_descriptor_descent(210.0), -210.0);
        assert_eq!(normalize_descriptor_descent(-210.0), -210.0);
        assert_eq!(normalize_descriptor_descent(0.0), 0.0);
    }

    #[test]
    fn font_weight_is_derived_from_pdf_font_names() {
        assert!(font_name_is_bold(b"HAYRHJ+LibertinusSerif-Bold-Identity-H"));
        assert!(font_name_is_bold(b"ABCDEE+SourceSans-SemiBold"));
        assert!(!font_name_is_bold(b"DQSIMB+LibertinusSerif-Regular"));
        assert!(!font_name_is_bold(b"DMFRRH+LibertinusSerif-Italic"));
    }

    #[test]
    fn embedded_type1_encoding_is_read_only_before_eexec() {
        let encoding = parse_type1_cleartext_encoding(
            b"%!PS-AdobeFont-1.0\n/Encoding 256 array\n\
              dup 65 /alpha put\ndup 66 /B put\nreadonly def\neexec\n\
              dup 67 /C put",
        )
        .unwrap();

        assert_eq!(encoding.unicode[65], Some('\u{03b1}'));
        assert_eq!(encoding.unicode[66], Some('B'));
        assert_eq!(encoding.unicode[67], None);
        assert_eq!(
            encoding.glyph_names[65].as_deref(),
            Some(b"alpha".as_slice())
        );
    }
}
