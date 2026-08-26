# ADR-0017: Translation degradation and strict mode

- Status: Accepted (2026-08-25)
- Decision level: Public because preservation, terminal reasons, and output publication are CLI
  contracts.

## Context

ADR-0013 established document, page, and paragraph degradation for PDF parsing and preservation.
M2 adds failures that occur while translating an otherwise usable paragraph: provider rejection,
timeout exhaustion, malformed output, and placeholder protocol violations. A provider may also
correctly return the request unchanged for symbols, numbers, addresses, or already-target-language
text. Treating that identity result as a protocol failure creates false degradation. Issue #30
requires normal mode to produce the safest useful document while `--strict` turns any real
recoverable degradation into a predictable hard failure.

The writer already publishes through a same-directory temporary file. Strict mode must make its
decision before that publisher is reached so it also preserves an existing destination.

## Decision

1. A translation error produced for one paragraph preserves that paragraph with
   `translation_failure`. Placeholder validation uses the narrower `placeholder_violation` reason
   and carries one stable subtype: `missing`, `duplicate`, `unknown`, `tag_nesting`,
   `partial_token`, or `formula_order`. Both failure classes leave `translated_text` empty, so
   Typeset creates no replacement for that paragraph.
2. A response byte-for-byte equal to the prepared request is `TranslationOutcome::Identity`, not a
   placeholder violation. The paragraph keeps source text as its translated result, emits an
   informational `translation_identity` diagnostic, may enter the separate identity cache, and
   does not count as degradation or a warning. Strict mode therefore accepts identity outcomes.
3. The pipeline counts identity outcomes only among prose-shaped requests (at least 40 characters
   and at least 50% ASCII letters). When identities exceed half of those requests, it emits one
   `suspicious_translation_echo_rate` warning. This warning is a document-level quality signal; it
   does not retroactively preserve paragraphs or fail strict mode.
4. Normal mode continues after paragraph failures and existing page/paragraph degradation. It emits
   the existing additive `degradation_summary` diagnostic, then a successful result. The result
   payload remains unchanged; its warning count includes the summary.
5. Failures outside a paragraph request remain document failures. This includes configuration,
   document-level term extraction, PDF parsing, assets, cache I/O, output construction, and atomic
   publication. Their existing exit categories remain authoritative.
6. `--strict` checks degradation after every successful pass and before that pass receives a debug
   snapshot or any later pass executes. It first records the complete degradation summary, then
   returns Translation/4 with reason `strict_degradation`. It never reaches Typeset/Write after an
   earlier degradation and therefore cannot create a temporary output or replace a destination.
7. CLI v2 adds `strict` to `configuration_resolved`; `placeholder_violation` and each corresponding
   `degradation_summary.preserved_paragraphs` entry expose the exact subtype additively. Debug mode
   may emit `translation_failure_profile` containing only response byte/character counts and token
   scan shape. It never stores response plaintext. Human and NDJSON modes consume the same
   diagnostics and terminal error.

## Consequences

- A long document can complete with isolated source paragraphs instead of losing all successful
  work because one request failed.
- Correct no-op translations no longer create false placeholder failures, while a suspicious
  document-wide echo pattern remains observable without exposing response text.
- Strict mode is suitable for automation that forbids recoverable degradation: exit 0 means no page
  or paragraph was preserved for a failure. It does not mean every paragraph differs from source.
- Diagnostics, rather than duplicated terminal-result fields, remain the source of page indices,
  paragraph indices, scopes, preservation reasons, and placeholder subtypes, preserving ADR-0011's
  result shape.
