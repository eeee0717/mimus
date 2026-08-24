use std::collections::BTreeSet;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_FORM_DEPTH: usize = 64;
const MAX_PAGE_TREE_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PageEvidence {
    pub visible_text_shows: usize,
    pub invisible_text_shows: usize,
    pub has_image: bool,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageClass {
    Blank,
    Scanned,
    Content,
}

impl PageEvidence {
    pub(crate) const fn classify(self) -> PageClass {
        if !self.complete {
            return PageClass::Content;
        }
        if self.visible_text_shows == 0 && self.invisible_text_shows == 0 && !self.has_image {
            PageClass::Blank
        } else if self.visible_text_shows == 0 && self.has_image {
            PageClass::Scanned
        } else {
            PageClass::Content
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ScanState {
    rendering_mode: u8,
    saved_rendering_modes: Vec<u8>,
    in_text_object: bool,
}

struct Scanner<'a> {
    document: &'a Document,
    evidence: PageEvidence,
    active_forms: BTreeSet<ObjectId>,
}

pub(crate) fn prescan_page(document: &Document, page_id: ObjectId) -> PageEvidence {
    let mut scanner = Scanner {
        document,
        evidence: PageEvidence {
            complete: true,
            ..PageEvidence::default()
        },
        active_forms: BTreeSet::new(),
    };
    scanner.scan_page(page_id);
    scanner.evidence
}

impl Scanner<'_> {
    fn scan_page(&mut self, page_id: ObjectId) {
        let Some(data) = self.page_content(page_id) else {
            return;
        };
        let resources = inherited_page_resources(self.document, page_id);
        if resources.is_err() {
            self.evidence.complete = false;
        }
        let mut state = ScanState::default();
        self.scan_content(&data, resources.ok().flatten().as_ref(), &mut state, 0);
    }

    fn page_content(&mut self, page_id: ObjectId) -> Option<Vec<u8>> {
        let page = match self.document.get_dictionary(page_id) {
            Ok(value) => value,
            Err(_) => {
                self.evidence.complete = false;
                return None;
            }
        };
        let contents = match page.get(b"Contents") {
            Ok(value) => value,
            Err(_) => return Some(Vec::new()),
        };
        let objects = match contents {
            Object::Array(values) => values.iter().collect::<Vec<_>>(),
            value => vec![value],
        };
        let mut data = Vec::new();
        for object in objects {
            let stream = match self.document.dereference(object) {
                Ok((_, value)) => match value.as_stream() {
                    Ok(stream) => stream,
                    Err(_) => {
                        self.evidence.complete = false;
                        continue;
                    }
                },
                Err(_) => {
                    self.evidence.complete = false;
                    continue;
                }
            };
            let remaining = MAX_CONTENT_BYTES.saturating_sub(data.len());
            let decoded = match stream.decompressed_content_with_limit(remaining) {
                Ok(value) => value,
                Err(_) => {
                    self.evidence.complete = false;
                    continue;
                }
            };
            if decoded.len() > remaining {
                self.evidence.complete = false;
                continue;
            }
            data.extend_from_slice(&decoded);
            data.push(b'\n');
        }
        Some(data)
    }

    fn scan_content(
        &mut self,
        data: &[u8],
        resources: Option<&Dictionary>,
        state: &mut ScanState,
        depth: usize,
    ) {
        let content = match Content::decode(data) {
            Ok(value) => value,
            Err(_) => {
                self.evidence.complete = false;
                return;
            }
        };
        if Content::decode_strict(data).is_err() {
            self.evidence.complete = false;
        }
        for operation in &content.operations {
            self.scan_operation(operation, resources, state, depth);
        }
        if state.in_text_object || !state.saved_rendering_modes.is_empty() {
            self.evidence.complete = false;
        }
    }

    fn scan_operation(
        &mut self,
        operation: &Operation,
        resources: Option<&Dictionary>,
        state: &mut ScanState,
        depth: usize,
    ) {
        match operation.operator.as_str() {
            "q" => {
                if !operation.operands.is_empty() {
                    self.evidence.complete = false;
                }
                state.saved_rendering_modes.push(state.rendering_mode);
            }
            "Q" => {
                if !operation.operands.is_empty() {
                    self.evidence.complete = false;
                }
                match state.saved_rendering_modes.pop() {
                    Some(mode) => state.rendering_mode = mode,
                    None => self.evidence.complete = false,
                }
            }
            "BT" => {
                if state.in_text_object || !operation.operands.is_empty() {
                    self.evidence.complete = false;
                }
                state.in_text_object = true;
            }
            "ET" => {
                if !state.in_text_object || !operation.operands.is_empty() {
                    self.evidence.complete = false;
                }
                state.in_text_object = false;
            }
            "Tr" => self.set_rendering_mode(operation, state),
            "Tj" | "'" => {
                let nonempty = (operation.operands.len() == 1)
                    .then(|| {
                        operation.operands[0]
                            .as_str()
                            .ok()
                            .map(|value| !value.is_empty())
                    })
                    .flatten();
                self.record_text_show(nonempty, state.in_text_object, state.rendering_mode);
            }
            "TJ" => {
                let nonempty = (operation.operands.len() == 1)
                    .then(|| {
                        operation.operands[0]
                            .as_array()
                            .ok()
                            .and_then(|values| text_array_nonempty(values))
                    })
                    .flatten();
                self.record_text_show(nonempty, state.in_text_object, state.rendering_mode);
            }
            "\"" => {
                let valid_numbers = operation
                    .operands
                    .get(..2)
                    .is_some_and(|values| values.iter().all(is_number));
                let nonempty = (operation.operands.len() == 3 && valid_numbers)
                    .then(|| {
                        operation.operands[2]
                            .as_str()
                            .ok()
                            .map(|value| !value.is_empty())
                    })
                    .flatten();
                self.record_text_show(nonempty, state.in_text_object, state.rendering_mode);
            }
            "BI" => {
                self.evidence.has_image = true;
                if operation.operands.len() != 1
                    || !matches!(operation.operands[0], Object::Stream(_))
                {
                    self.evidence.complete = false;
                }
            }
            "Do" => self.scan_xobject(operation, resources, state.rendering_mode, depth),
            _ => {}
        }
    }

    fn set_rendering_mode(&mut self, operation: &Operation, state: &mut ScanState) {
        if !state.in_text_object || operation.operands.len() != 1 {
            self.evidence.complete = false;
            return;
        }
        let Ok(value) = operation.operands[0].as_i64() else {
            self.evidence.complete = false;
            return;
        };
        let Ok(mode) = u8::try_from(value) else {
            self.evidence.complete = false;
            return;
        };
        if mode > 7 {
            self.evidence.complete = false;
            return;
        }
        state.rendering_mode = mode;
    }

    fn record_text_show(
        &mut self,
        nonempty: Option<bool>,
        in_text_object: bool,
        rendering_mode: u8,
    ) {
        if !in_text_object {
            self.evidence.complete = false;
        }
        let Some(nonempty) = nonempty else {
            self.evidence.complete = false;
            return;
        };
        if !nonempty {
            return;
        }
        if matches!(rendering_mode, 3 | 7) {
            self.evidence.invisible_text_shows += 1;
        } else {
            self.evidence.visible_text_shows += 1;
        }
    }

    fn scan_xobject(
        &mut self,
        operation: &Operation,
        resources: Option<&Dictionary>,
        rendering_mode: u8,
        depth: usize,
    ) {
        if operation.operands.len() != 1 {
            self.evidence.complete = false;
            return;
        }
        let Ok(name) = operation.operands[0].as_name() else {
            self.evidence.complete = false;
            return;
        };
        let Some(resources) = resources else {
            self.evidence.complete = false;
            return;
        };
        let xobjects = match resources
            .get_deref(b"XObject", self.document)
            .and_then(Object::as_dict)
        {
            Ok(value) => value,
            Err(_) => {
                self.evidence.complete = false;
                return;
            }
        };
        let object = match xobjects.get(name) {
            Ok(value) => value,
            Err(_) => {
                self.evidence.complete = false;
                return;
            }
        };
        let (object_id, object) = match self.document.dereference(object) {
            Ok(value) => value,
            Err(_) => {
                self.evidence.complete = false;
                return;
            }
        };
        let stream = match object.as_stream() {
            Ok(value) => value,
            Err(_) => {
                self.evidence.complete = false;
                return;
            }
        };
        match stream.dict.get(b"Subtype").and_then(Object::as_name) {
            Ok(b"Image") => self.evidence.has_image = true,
            Ok(b"Form") => self.scan_form(stream, object_id, resources, rendering_mode, depth),
            _ => self.evidence.complete = false,
        }
    }

    fn scan_form(
        &mut self,
        stream: &Stream,
        object_id: Option<ObjectId>,
        parent_resources: &Dictionary,
        rendering_mode: u8,
        depth: usize,
    ) {
        if depth >= MAX_FORM_DEPTH {
            self.evidence.complete = false;
            return;
        }
        if object_id.is_some_and(|id| !self.active_forms.insert(id)) {
            self.evidence.complete = false;
            return;
        }
        let resources = match stream.dict.get(b"Resources") {
            Ok(value) => match self
                .document
                .dereference(value)
                .and_then(|(_, value)| value.as_dict())
            {
                Ok(value) => value,
                Err(_) => {
                    self.evidence.complete = false;
                    if let Some(id) = object_id {
                        self.active_forms.remove(&id);
                    }
                    return;
                }
            },
            Err(_) => parent_resources,
        };
        match stream.decompressed_content_with_limit(MAX_CONTENT_BYTES) {
            Ok(data) => {
                let mut state = ScanState {
                    rendering_mode,
                    ..ScanState::default()
                };
                self.scan_content(&data, Some(resources), &mut state, depth + 1);
            }
            Err(_) => self.evidence.complete = false,
        }
        if let Some(id) = object_id {
            self.active_forms.remove(&id);
        }
    }
}

fn inherited_page_resources(
    document: &Document,
    page_id: ObjectId,
) -> Result<Option<Dictionary>, ()> {
    let mut current = page_id;
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_PAGE_TREE_DEPTH {
        if !visited.insert(current) {
            return Err(());
        }
        let dictionary = document
            .get_object(current)
            .and_then(Object::as_dict)
            .map_err(|_| ())?;
        if let Ok(resources) = dictionary.get(b"Resources") {
            return document
                .dereference(resources)
                .and_then(|(_, value)| value.as_dict())
                .map(|value| Some(value.clone()))
                .map_err(|_| ());
        }
        current = match dictionary.get(b"Parent") {
            Ok(value) => value.as_reference().map_err(|_| ())?,
            Err(_) => return Ok(None),
        };
    }
    Err(())
}

fn is_number(value: &Object) -> bool {
    matches!(value, Object::Integer(_) | Object::Real(_))
}

fn text_array_nonempty(values: &[Object]) -> Option<bool> {
    let mut nonempty = false;
    for value in values {
        match value {
            Object::String(bytes, _) => nonempty |= !bytes.is_empty(),
            value if is_number(value) => {}
            _ => return None,
        }
    }
    Some(nonempty)
}

#[cfg(test)]
mod tests {
    use lopdf::{Object, Stream, dictionary};

    use super::*;

    fn page_with_content(
        document: &mut Document,
        content: &[u8],
        resources: Dictionary,
    ) -> ObjectId {
        let content_id = document.add_object(Stream::new(Dictionary::new(), content.to_vec()));
        let resources_id = document.add_object(resources);
        document.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => content_id,
            "Resources" => resources_id,
        })
    }

    fn image(document: &mut Document) -> ObjectId {
        document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 1,
                "Height" => 1,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8,
            },
            vec![0, 0, 0],
        ))
    }

    fn form(document: &mut Document, content: &[u8], resources: Dictionary) -> ObjectId {
        document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
                "Resources" => resources,
            },
            content.to_vec(),
        ))
    }

    #[test]
    fn rendering_modes_three_and_seven_are_the_only_invisible_modes() {
        for mode in 0..=7 {
            let mut document = Document::with_version("1.7");
            let content = format!("BT {mode} Tr (x) Tj ET");
            let page = page_with_content(&mut document, content.as_bytes(), Dictionary::new());

            let evidence = prescan_page(&document, page);

            assert!(evidence.complete, "Tr {mode}");
            assert_eq!(
                evidence.visible_text_shows,
                usize::from(!matches!(mode, 3 | 7))
            );
            assert_eq!(
                evidence.invisible_text_shows,
                usize::from(matches!(mode, 3 | 7))
            );
            assert_eq!(evidence.classify(), PageClass::Content);
        }
    }

    #[test]
    fn non_upright_text_still_counts_as_visible_scan_evidence() {
        let mut document = Document::with_version("1.7");
        let page = page_with_content(
            &mut document,
            b"BT 0 1 -1 0 100 100 Tm (rotated) Tj ET",
            Dictionary::new(),
        );

        let evidence = prescan_page(&document, page);

        assert_eq!(evidence.visible_text_shows, 1);
        assert_eq!(evidence.invisible_text_shows, 0);
        assert_eq!(evidence.classify(), PageClass::Content);
    }

    #[test]
    fn graphics_state_save_and_restore_restores_the_rendering_mode() {
        let mut document = Document::with_version("1.7");
        let page = page_with_content(
            &mut document,
            b"BT 3 Tr ET q BT 0 Tr (visible) Tj ET Q BT (hidden) Tj ET",
            Dictionary::new(),
        );

        let evidence = prescan_page(&document, page);

        assert_eq!(
            evidence,
            PageEvidence {
                visible_text_shows: 1,
                invisible_text_shows: 1,
                has_image: false,
                complete: true,
            }
        );
    }

    #[test]
    fn inline_images_are_scanned_page_evidence() {
        let mut document = Document::with_version("1.7");
        let page = page_with_content(
            &mut document,
            b"BI /W 1 /H 1 /CS /RGB /BPC 8 ID \x00\x00\x00 EI",
            Dictionary::new(),
        );

        let evidence = prescan_page(&document, page);

        assert!(evidence.complete);
        assert!(evidence.has_image);
        assert_eq!(evidence.classify(), PageClass::Scanned);
    }

    #[test]
    fn nested_forms_use_their_own_resource_scope_to_find_images() {
        let mut document = Document::with_version("1.7");
        let image = image(&mut document);
        let inner = form(
            &mut document,
            b"/Im Do",
            dictionary! { "XObject" => dictionary! { "Im" => image } },
        );
        let outer = form(
            &mut document,
            b"/Inner Do",
            dictionary! { "XObject" => dictionary! { "Inner" => inner } },
        );
        let page = page_with_content(
            &mut document,
            b"/Outer Do",
            dictionary! { "XObject" => dictionary! { "Outer" => outer } },
        );

        let evidence = prescan_page(&document, page);

        assert!(evidence.complete);
        assert!(evidence.has_image);
        assert_eq!(evidence.classify(), PageClass::Scanned);
    }

    #[test]
    fn form_cycles_and_depth_overflow_degrade_to_content() {
        let mut cyclic = Document::with_version("1.7");
        let a = cyclic.new_object_id();
        let b = cyclic.new_object_id();
        cyclic.objects.insert(
            a,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
                    "Resources" => dictionary! { "XObject" => dictionary! { "B" => b } },
                },
                b"/B Do".to_vec(),
            )),
        );
        cyclic.objects.insert(
            b,
            Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
                    "Resources" => dictionary! { "XObject" => dictionary! { "A" => a } },
                },
                b"/A Do".to_vec(),
            )),
        );
        let page = page_with_content(
            &mut cyclic,
            b"/A Do",
            dictionary! { "XObject" => dictionary! { "A" => a } },
        );
        let evidence = prescan_page(&cyclic, page);
        assert!(!evidence.complete);
        assert_eq!(evidence.classify(), PageClass::Content);

        let mut deep = Document::with_version("1.7");
        let mut next = image(&mut deep);
        for _ in 0..=MAX_FORM_DEPTH {
            next = form(
                &mut deep,
                b"/Next Do",
                dictionary! { "XObject" => dictionary! { "Next" => next } },
            );
        }
        let page = page_with_content(
            &mut deep,
            b"/Next Do",
            dictionary! { "XObject" => dictionary! { "Next" => next } },
        );
        let evidence = prescan_page(&deep, page);
        assert!(!evidence.complete);
        assert_eq!(evidence.classify(), PageClass::Content);
    }

    #[test]
    fn malformed_or_incomplete_evidence_never_becomes_blank_or_scanned() {
        let mut malformed_text = Document::with_version("1.7");
        let page = page_with_content(&mut malformed_text, b"BT 42 Tj ET", Dictionary::new());
        let evidence = prescan_page(&malformed_text, page);
        assert!(!evidence.complete);
        assert_eq!(evidence.classify(), PageClass::Content);

        let mut unbalanced = Document::with_version("1.7");
        let image = image(&mut unbalanced);
        let page = page_with_content(
            &mut unbalanced,
            b"q /Im Do",
            dictionary! { "XObject" => dictionary! { "Im" => image } },
        );
        let evidence = prescan_page(&unbalanced, page);
        assert!(evidence.has_image);
        assert!(!evidence.complete);
        assert_eq!(evidence.classify(), PageClass::Content);
    }
}
