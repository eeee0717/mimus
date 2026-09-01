# ADR-0017: Translation degradation and strict mode

- Status: Accepted (2026-08-25)
- Decision level: Public because preservation, terminal reasons, and output publication are CLI
  contracts.
- Revision: 2026-08-31 accepts whitespace-only and non-alphabetic numeric/symbol source shapes as
  local identities before either translation backend.

## Context

ADR-0013 established document, page, and paragraph degradation for PDF parsing and preservation.
M2 adds failures that occur while translating an otherwise usable paragraph: provider rejection,
timeout exhaustion, malformed output, and placeholder protocol violations. A provider may also
correctly return the request unchanged for symbols, numbers, addresses, or already-target-language
text. Treating that identity result as a protocol failure creates false degradation. Issue #30
requires normal mode to produce the safest useful document while `--strict` turns any real
recoverable degradation into a predictable hard failure.

The M3 real-paper run also showed that a provider can lazily echo translation-worthy prose while
still returning valid placeholder structure, and that one paragraph can repeatedly fail the same
placeholder-order check. The writer already publishes through a same-directory temporary file.
Strict mode must make its decision before that publisher is reached so it also preserves an
existing destination.

## Decision

1. A translation error produced for one paragraph preserves that paragraph with
   `translation_failure`. Placeholder validation uses the narrower `placeholder_violation` reason
   and carries one stable subtype: `missing`, `duplicate`, `unknown`, `tag_nesting`,
   `partial_token`, or `formula_order`. Both failure classes leave `translated_text` empty, so
   Typeset creates no replacement for that paragraph.
2. Every placeholder violation subtype gets one semantic retry after validation. This budget is
   independent of transport retries and of the echo retry below. A second invalid response follows
   the existing paragraph degradation path. The retry request adds a correction scoped to the
   observed subtype: it names missing, duplicated, or unknown tokens, gives the required formula or
   bold-tag order, or requires complete tokens. It does not change the source request, cache key, or
   validator. Invalid responses remain absent from the cache; the retry emits paragraph-scoped
   `placeholder_retry` with the subtype and semantic response attempt. Alternating echo and
   placeholder failures can therefore produce at most three semantic responses for one paragraph.
3. Whitespace-only source and source with no alphabetic characters are local identity shapes. They
   do not enter automatic term extraction, either translation backend, or the translation cache;
   they retain the source operand and produce no Typeset ink. For remaining requests, a response
   byte-for-byte equal to the prepared request is `TranslationOutcome::Identity`, not a placeholder
   violation. A translation-worthy shape (any alphabetic content except an email address) gets one
   semantic retry. If the second response is also an echo, the paragraph keeps source text as its
   translated result, emits informational `translation_identity` plus the paragraph-scoped
   `suspicious_echo` warning, enters the separate identity cache, and appears in
   `degradation_summary.suspicious_echoes`. Email-shaped requests keep the original one-response
   backend identity behavior and emit neither diagnostic; expected identity shapes do not consume
   the diagnostic budget.
4. `suspicious_echo` is visible quality evidence, not hard degradation. It does not set
   `Paragraph.preserved`, does not block output, and does not make `--strict` fail. Strict output
   still lists it through the ordinary diagnostic stream and degradation summary, so automation or
   human review can apply a stronger quality policy without conflating a possible correct no-op
   with a mechanically invalid response.
5. The pipeline counts identity outcomes only among prose-shaped requests (at least 40 characters
   and at least 50% ASCII letters). When identities exceed half of those requests, it emits one
   `suspicious_translation_echo_rate` warning. This warning is a document-level quality signal; it
   does not retroactively preserve paragraphs or fail strict mode.
6. Normal mode continues after paragraph failures and existing page/paragraph degradation. It emits
   the existing additive `degradation_summary` diagnostic, then a successful result. The result
   payload remains unchanged; its warning count includes the summary.
7. Failures outside a paragraph request remain document failures. This includes configuration,
   document-level term extraction, PDF parsing, assets, cache I/O, output construction, and atomic
   publication. Their existing exit categories remain authoritative.
8. `--strict` checks degradation after every successful pass and before that pass receives a debug
   snapshot or any later pass executes. It first records the complete degradation summary, then
   returns Translation/4 with reason `strict_degradation`. It never reaches Typeset/Write after an
   earlier degradation and therefore cannot create a temporary output or replace a destination.
9. CLI v2 adds `strict` to `configuration_resolved`; `placeholder_violation` and each corresponding
   `degradation_summary.preserved_paragraphs` entry expose the exact subtype additively. Debug mode
   may emit `translation_failure_profile` containing only response byte/character counts and token
   scan shape. It never stores response plaintext. Human and NDJSON modes consume the same
   diagnostics and terminal error.

## Consequences

- A long document can complete with isolated source paragraphs instead of losing all successful
  work because one request failed.
- Empty and non-alphabetic source fragments cannot consume provider calls or Typeset capacity by
  receiving synthetic prose from a backend.
- Correct no-op translations no longer create false placeholder failures. Translation-worthy
  repeated echoes are visible per paragraph and document-wide without exposing response text.
- A first malformed semantic response can recover without degrading the paragraph. A persistent
  violation costs two provider responses per uncached paragraph and remains deliberately uncached.
- Strict mode is suitable for automation that forbids recoverable degradation: exit 0 means no page
  or paragraph was preserved for a failure. It does not mean every paragraph differs from source.
- Diagnostics, rather than duplicated terminal-result fields, remain the source of page indices,
  paragraph indices, scopes, preservation reasons, and placeholder subtypes, preserving ADR-0011's
  result shape.
