use std::collections::BTreeSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::error::{InputReason, MimusError};
use crate::event::PageDegradeReason;
use crate::il::{PageGeometry, Rect};

const MAX_PAGE_TREE_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PageFrame {
    pub media_box: Rect,
    pub crop_box: Rect,
    pub raw_rotate_degrees: i32,
    pub rotate_degrees: i32,
}

impl PageFrame {
    pub(crate) fn resolve(
        document: &Document,
        page_id: ObjectId,
    ) -> std::result::Result<Self, PageGeometryResolveError> {
        let properties = inherited_properties(document, page_id)?;
        let media_box = parse_box(
            document,
            properties.media_box.as_ref().ok_or_else(|| {
                degraded(
                    PageDegradeReason::BadPageGeometry,
                    "page tree has no inherited MediaBox",
                )
            })?,
            "MediaBox",
        )?;
        let crop_box = match properties.crop_box.as_ref() {
            Some(value) => parse_box(document, value, "CropBox")?,
            None => media_box,
        };
        if !contains(media_box, crop_box) {
            return Err(degraded(
                PageDegradeReason::BadPageGeometry,
                "CropBox is not contained by MediaBox",
            ));
        }
        let raw_rotate_degrees = match properties.rotate.as_ref() {
            Some(value) => parse_rotation(document, value)?,
            None => 0,
        };
        if raw_rotate_degrees % 90 != 0 {
            return Err(degraded(
                PageDegradeReason::UnsupportedRotation,
                format!("page /Rotate {raw_rotate_degrees} is not a multiple of 90"),
            ));
        }
        Ok(Self {
            media_box,
            crop_box,
            raw_rotate_degrees,
            rotate_degrees: raw_rotate_degrees.rem_euclid(360),
        })
    }

    pub(crate) fn geometry(self) -> PageGeometry {
        let width = self.crop_box.right - self.crop_box.left;
        let height = self.crop_box.top - self.crop_box.bottom;
        let (width, height) = if matches!(self.rotate_degrees, 90 | 270) {
            (height, width)
        } else {
            (width, height)
        };
        PageGeometry {
            width,
            height,
            rotate_degrees: self.rotate_degrees,
        }
    }
}

#[derive(Debug)]
pub(crate) enum PageGeometryResolveError {
    Degraded {
        reason: PageDegradeReason,
        source: MimusError,
    },
    Fatal(MimusError),
}

#[derive(Default)]
struct InheritedProperties {
    media_box: Option<Object>,
    crop_box: Option<Object>,
    rotate: Option<Object>,
}

fn inherited_properties(
    document: &Document,
    page_id: ObjectId,
) -> std::result::Result<InheritedProperties, PageGeometryResolveError> {
    let mut properties = InheritedProperties::default();
    let mut current = page_id;
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_PAGE_TREE_DEPTH {
        if !visited.insert(current) {
            return Err(fatal(format!(
                "page geometry inheritance contains a cycle at object {}",
                current.0
            )));
        }
        let dictionary = document
            .get_object(current)
            .and_then(Object::as_dict)
            .map_err(|error| {
                fatal(format!(
                    "invalid page tree object {} while resolving geometry: {error}",
                    current.0
                ))
            })?;
        inherit_once(&mut properties.media_box, dictionary, b"MediaBox");
        inherit_once(&mut properties.crop_box, dictionary, b"CropBox");
        inherit_once(&mut properties.rotate, dictionary, b"Rotate");

        if !dictionary.has(b"Parent") {
            return Ok(properties);
        }
        current = dictionary
            .get(b"Parent")
            .and_then(Object::as_reference)
            .map_err(|error| fatal(format!("page tree has an invalid Parent: {error}")))?;
    }
    Err(fatal("page geometry inheritance exceeds 128 levels"))
}

fn inherit_once(target: &mut Option<Object>, dictionary: &Dictionary, key: &[u8]) {
    if target.is_none() {
        *target = dictionary.get(key).ok().cloned();
    }
}

fn parse_box(
    document: &Document,
    object: &Object,
    name: &str,
) -> std::result::Result<Rect, PageGeometryResolveError> {
    let (_, object) = document
        .dereference(object)
        .map_err(|error| fatal(format!("could not dereference inherited {name}: {error}")))?;
    let values = object.as_array().map_err(|error| {
        degraded(
            PageDegradeReason::BadPageGeometry,
            format!("{name} is not an array: {error}"),
        )
    })?;
    if values.len() != 4 {
        return Err(degraded(
            PageDegradeReason::BadPageGeometry,
            format!("{name} must contain exactly four values"),
        ));
    }
    let mut numbers = [0.0; 4];
    for (index, value) in values.iter().enumerate() {
        let (_, value) = document.dereference(value).map_err(|error| {
            fatal(format!(
                "could not dereference {name} value {index}: {error}"
            ))
        })?;
        numbers[index] = object_number(value).ok_or_else(|| {
            degraded(
                PageDegradeReason::BadPageGeometry,
                format!("{name} value {index} is not numeric"),
            )
        })?;
    }
    if !numbers.iter().all(|value| value.is_finite())
        || numbers[2] <= numbers[0]
        || numbers[3] <= numbers[1]
    {
        return Err(degraded(
            PageDegradeReason::BadPageGeometry,
            format!("{name} is non-finite or degenerate"),
        ));
    }
    Ok(Rect {
        left: numbers[0],
        bottom: numbers[1],
        right: numbers[2],
        top: numbers[3],
    })
}

fn parse_rotation(
    document: &Document,
    object: &Object,
) -> std::result::Result<i32, PageGeometryResolveError> {
    let (_, object) = document
        .dereference(object)
        .map_err(|error| fatal(format!("could not dereference inherited Rotate: {error}")))?;
    let value = object.as_i64().map_err(|error| {
        degraded(
            PageDegradeReason::UnsupportedRotation,
            format!("page /Rotate is not an integer: {error}"),
        )
    })?;
    i32::try_from(value).map_err(|_| {
        degraded(
            PageDegradeReason::UnsupportedRotation,
            format!("page /Rotate {value} is outside the supported integer range"),
        )
    })
}

fn object_number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.left >= outer.left
        && inner.bottom >= outer.bottom
        && inner.right <= outer.right
        && inner.top <= outer.top
}

fn degraded(reason: PageDegradeReason, message: impl Into<String>) -> PageGeometryResolveError {
    PageGeometryResolveError::Degraded {
        reason,
        source: MimusError::input(InputReason::PdfParse, message),
    }
}

fn fatal(message: impl Into<String>) -> PageGeometryResolveError {
    PageGeometryResolveError::Fatal(MimusError::input(InputReason::PdfParse, message))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lopdf::{Stream, dictionary};

    use super::*;

    fn fixture_path(id: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures")
            .join(id)
            .join(format!("{id}.pdf"))
    }

    #[test]
    fn resolves_crop_size_and_normalizes_quarter_turns() {
        for (id, expected) in [
            ("unit-geom-01-rotate-0", (300.0, 200.0, 0)),
            ("unit-geom-01-rotate-90", (200.0, 300.0, 90)),
            ("unit-geom-01-rotate-180", (300.0, 200.0, 180)),
            ("unit-geom-01-rotate-270", (200.0, 300.0, 270)),
            ("unit-geom-01-rotate-neg90", (200.0, 300.0, 270)),
        ] {
            let document = Document::load(fixture_path(id)).unwrap();
            let page_id = document.get_pages()[&1];
            let geometry = PageFrame::resolve(&document, page_id).unwrap().geometry();
            assert_eq!(
                (geometry.width, geometry.height, geometry.rotate_degrees),
                expected,
                "fixture {id}"
            );
        }

        let document = Document::load(fixture_path("unit-geom-05-nonzero-origin-boxes")).unwrap();
        let page_id = document.get_pages()[&1];
        let frame = PageFrame::resolve(&document, page_id).unwrap();
        assert_eq!(frame.media_box.left, 100.0);
        assert_eq!(frame.crop_box.left, 120.0);
        assert_eq!(frame.geometry().width, 260.0);
        assert_eq!(frame.geometry().height, 160.0);
    }

    #[test]
    fn resolves_inherited_and_indirect_page_properties_as_objects() {
        let mut document = Document::with_version("1.7");
        let media_box = document.add_object(vec![0.into(), 0.into(), 612.into(), 792.into()]);
        let crop_box = document.add_object(vec![50.into(), 50.into(), 562.into(), 742.into()]);
        let parent = document.add_object(dictionary! {
            "Type" => "Pages",
            "MediaBox" => media_box,
            "CropBox" => crop_box,
            "Rotate" => -90,
        });
        let contents = document.add_object(Stream::new(Dictionary::new(), Vec::new()));
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => parent,
            "Contents" => contents,
        });

        let frame = PageFrame::resolve(&document, page).unwrap();
        assert_eq!(frame.raw_rotate_degrees, -90);
        assert_eq!(frame.rotate_degrees, 270);
        assert_eq!(frame.crop_box.left, 50.0);
        assert_eq!(frame.geometry().width, 692.0);
        assert_eq!(frame.geometry().height, 512.0);
    }

    #[test]
    fn malformed_boxes_and_rotations_are_page_degradations() {
        for (key, value, reason) in [
            (
                "MediaBox",
                Object::Array(vec![0.into(), Object::Null, 300.into(), 200.into()]),
                PageDegradeReason::BadPageGeometry,
            ),
            (
                "Rotate",
                Object::Integer(45),
                PageDegradeReason::UnsupportedRotation,
            ),
        ] {
            let mut document = Document::with_version("1.7");
            let mut page = dictionary! {
                "Type" => "Page",
                "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
            };
            page.set(key, value);
            let page = document.add_object(page);
            assert!(matches!(
                PageFrame::resolve(&document, page),
                Err(PageGeometryResolveError::Degraded {
                    reason: actual,
                    ..
                }) if actual == reason
            ));
        }
    }

    #[test]
    fn page_parent_cycles_are_document_errors() {
        let mut document = Document::with_version("1.7");
        let page = document.new_object_id();
        document.objects.insert(
            page,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => page,
                "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
            }),
        );
        assert!(matches!(
            PageFrame::resolve(&document, page),
            Err(PageGeometryResolveError::Fatal(_))
        ));
    }
}
