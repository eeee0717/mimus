# Layout detector recordings

These versioned JSON files replay the PP-DocLayoutV3 outcomes adjudicated in
`docs/04-m0-experiment-1.md`. They are deterministic CI inputs for the
`LayoutDetector` boundary; ordinary CI does not download or execute the 131 MB
model.

The model labels and confidence values come from the M0 experiment. Region
coordinates are pinned in PDF visual-page points against the corresponding
Corpus fixture. A recording is test data, not an oracle: the hand-written
fixture manifest and independent PDF tools remain the acceptance source.

Real-model qualification is explicit and local/ignored. Updating a recording
requires rerunning the pinned model, recording the raw outcome, and adjudicating
any difference without changing the expected manifest to match the model.
