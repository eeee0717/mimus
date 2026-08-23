use std::collections::BTreeSet;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_FORM_DEPTH: usize = 32;
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
