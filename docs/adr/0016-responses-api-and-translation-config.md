# ADR-0016: Responses API and translation configuration

- Status: Accepted (2026-08-25)
- Decision level: Public and difficult to reverse because environment names, config keys, and the
  HTTP path are user-facing contracts.

## Context

M2 introduces the first network translation backend. Issue #25 requires OpenAI-compatible endpoint,
model, target-language, and secret configuration with field-wise `flags > environment > user config`
precedence. The integration for this milestone must use the OpenAI Responses API rather than Chat
Completions.

The workspace also needs a local `.env.local` development file without committing its contents.
Tests must never read a real user key or contact a public endpoint.

## Decision

1. The OpenAI-compatible backend sends `POST` requests to the Responses API. A configured base URL
   is normalized as follows: `/v1/responses` remains unchanged, `/v1` gains `/responses`, and any
   other base path gains `/v1/responses`. No production code calls `/chat/completions`.
2. The request uses Responses fields `model`, `instructions`, and `input`. Output is accepted only
   from `output_text` content (including the top-level compatibility convenience field). Empty or
   malformed output is a typed translation failure.
3. CLI flags are `--endpoint`, `--model`, and `--target-language`. Environment aliases are:
   `MIMUS_OPENAI_BASE_URL` / `OPENAI_BASE_URL` / `BASE_URL`,
   `MIMUS_OPENAI_MODEL` / `OPENAI_MODEL` / `MODEL_ID`, and
   `MIMUS_TARGET_LANGUAGE` / `TARGET_LANGUAGE`. The user config keys are `base_url` (alias
   `endpoint`), `model` (alias `model_id`), and `target_language`.
4. API keys have no CLI flag. They resolve from
   `MIMUS_OPENAI_API_KEY` / `OPENAI_API_KEY` / `API_KEY`, then `api_key` in the user config. Empty
   values are absent. The default config path is `~/.config/mimus/config.toml`, with
   `XDG_CONFIG_HOME` and the test-only `MIMUS_CONFIG_FILE` override honored.
5. `.env.local` is loaded without replacing variables already present in the process environment.
   It is gitignored. Errors, events, diagnostics, and debug output never include request headers,
   response bodies, request source text, or the key.
6. Successful resolution emits the additive CLI v2 `configuration_resolved` event. It contains
   endpoint, model, backend, and target language but never secret material.

## Consequences

- OpenAI, compatible gateways, and local servers share one backend contract, but a service that only
  implements Chat Completions is intentionally unsupported.
- Generic aliases (`BASE_URL`, `MODEL_ID`, `API_KEY`) support the requested local environment file;
  prefixed names remain preferable in shared shells.
- HTTP errors are deliberately less verbose than raw provider errors. This prevents secret and
  source leakage; future provider detail must first pass an explicit redaction boundary.
