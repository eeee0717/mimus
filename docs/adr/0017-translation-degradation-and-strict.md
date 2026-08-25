# ADR-0017: Translation degradation and strict mode

- Status: Accepted (2026-08-25)
- Decision level: Public because preservation, terminal reasons, and output publication are CLI
  contracts.

## Context

ADR-0013 established document, page, and paragraph degradation for PDF parsing and preservation.
M2 adds failures that occur while translating an otherwise usable paragraph: provider rejection,
timeout exhaustion, malformed output, and placeholder protocol violations. Issue #30 requires normal
mode to produce the safest useful document while `--strict` turns any recoverable degradation into a
predictable hard failure.

The writer already publishes through a same-directory temporary file. Strict mode must make its
decision before that publisher is reached so it also preserves an existing destination.

## Decision

1. A translation error produced for one paragraph preserves that paragraph with
   `translation_failure`. Placeholder validation uses the narrower `placeholder_violation` reason.
   Both leave `translated_text` empty, so Typeset creates no replacement for that paragraph.
2. Normal mode continues after paragraph failures and existing page/paragraph degradation. It emits
   the existing additive `degradation_summary` diagnostic, then a successful result. The result
   payload remains unchanged; its warning count includes the summary.
3. Failures outside a paragraph request remain document failures. This includes configuration,
   document-level term extraction, PDF parsing, assets, cache I/O, output construction, and atomic
   publication. Their existing exit categories remain authoritative.
4. `--strict` checks degradation after every successful pass and before that pass receives a debug
   snapshot or any later pass executes. It first records the complete degradation summary, then
   returns Translation/4 with reason `strict_degradation`. It never reaches Typeset/Write after an
   earlier degradation and therefore cannot create a temporary output or replace a destination.
5. CLI v2 adds `strict` to `configuration_resolved` and adds the two typed reason values above. Human
   and NDJSON modes consume the same diagnostics and terminal error.

## Consequences

- A long document can complete with isolated source paragraphs instead of losing all successful
  work because one request failed.
- Strict mode is suitable for automation that requires an all-translated document: exit 0 means no
  page or paragraph was preserved.
- Diagnostics, rather than duplicated terminal-result fields, remain the source of page indices,
  paragraph indices, scopes, and preservation reasons, preserving ADR-0011's result shape.
