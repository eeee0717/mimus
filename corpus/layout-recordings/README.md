# Layout detector recordings

These versioned JSON files replay the PP-DocLayoutV3 outcomes adjudicated in
`docs/04-m0-experiment-1.md`. They are deterministic CI inputs for the
`LayoutDetector` boundary; ordinary CI does not download or execute the 131 MB
model.

The model labels and confidence values come from the M0 experiment. Region
coordinates are pinned in PDF visual-page points against the corresponding
Corpus fixture. A recording is test data, not an oracle: the hand-written
fixture manifest and independent PDF tools remain the acceptance source.

Real-model qualification is explicit, local, and env-gated. Updating a recording
requires rerunning the pinned model, recording the raw outcome, and adjudicating
any difference without changing the expected manifest to match the model.

`pp-doclayoutv3-unit-order-01-natural.json` was regenerated for #84 from the pinned raster in
`crates/mimus-core/tests/fixtures/pp-doclayoutv3/`. Its six bounds are the raw M0
pixel boxes converted from the 1167 x 612 raster to the fixture's 420 x 220 point
visual page. The original query ids are retained as `reading_order`. Independent
acceptance compared the order to `adjudicated.toml` and the raw rows quoted in
`docs/04-m0-experiment-1.md`; all six regions are `text` and follow L1-L3, R1-R3.
The qualification prefix intentionally prevents fixture-id auto-discovery from
changing the existing Corpus production baseline.

`unit-para-17-title-author-reordered.json` is the generated author-column fixture's adjudicated
policy overlay for the page-zero geometry regression. Its abstract has model order 2 while the two
author regions have orders 11 and 12; a separate `text` region lies geometrically above the title
band. This proves author protection and its negative boundary without treating reading order as
the author-block delimiter.
