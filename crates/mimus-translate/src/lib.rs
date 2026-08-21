//! Translation.
//!
//! A paragraph is serialised to text with formulas and styled runs replaced by
//! placeholders, sent to the model, then reassembled. Two failure modes are
//! worth designing against from the start, because both are common enough to
//! hit on the first real document:
//!
//! - the model drops or duplicates a placeholder;
//! - the model returns the input unchanged.
//!
//! Both need to degrade to "leave the original text in place", never to
//! "silently emit a paragraph with a missing formula".

use mimus_ir::Paragraph;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placeholder {
    Formula(u32),
    RichTextOpen(u32),
    RichTextClose(u32),
}

pub trait Translator {
    fn translate(&self, text: &str, from: &str, to: &str) -> anyhow::Result<String>;
}

/// Round-trip a paragraph through a translator, restoring placeholders.
pub fn translate_paragraph(
    _p: &mut Paragraph,
    _t: &dyn Translator,
) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}
