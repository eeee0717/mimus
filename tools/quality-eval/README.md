# Quality evaluation tools

These tools are offline evaluators and loopback test infrastructure. They do not alter the mimus
production CLI.

## Conserving fake server

`fake_responses.py` defaults to the frozen legacy behavior. `--mode conserving` replaces prose
letters deterministically while retaining placeholders, punctuation, numbers, numeric-context
units, and bracketed references. Bind is hard-coded to `127.0.0.1`; use it only with a local mimus
endpoint.

## Reference-free QE

The Python 3.11 environment is hash-locked:

```sh
python3.11 -m venv .venv
.venv/bin/pip install --require-hashes -r tools/quality-eval/requirements.lock.txt
```

This installation is one of the two allowed evaluation-time network classes. The other is the first
model download into the user Hugging Face cache. Paper downloads and real translation API calls are
not part of this workflow.

The default public model is `Unbabel/wmt20-comet-qe-da`. Anonymous access to the requested
`Unbabel/wmt22-cometkiwi-da` returned HTTP 401 on 2026-08-30, so the public reference-free model was
selected and is disclosed by source, revision, checkpoint SHA-256, and snapshot-tree SHA-256 in every
sidecar. Formula units and placeholders are stripped deterministically before scoring. QE remains
separate from the six-dimensional score. Pass `--expected-model-tree-sha256 <sha256>` to reject a
cache snapshot that differs from the reviewed asset before model loading.

## Cluster harness

`run_cluster.py` runs the existing archived inputs against the conserving fake server, measures each
published output, preserves real `Internal/6` rows as failures, and reruns ResNet plus repliable onion
routing for byte-level reproducibility. Reproducibility reruns omit debug and scorecard artifacts and
hash only the published PDF, so temporary-disk capacity is not part of the oracle. `--resume` reuses
both published reports and retained Internal failure rows. The harness emits `cluster-summary.json`
and a readable Markdown matrix. Per-page timing is N/A until public artifacts expose it.
