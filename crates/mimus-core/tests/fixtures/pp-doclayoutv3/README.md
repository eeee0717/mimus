# PP-DocLayoutV3 qualification raster

`unit-order-01-natural-200dpi.png` is the M0 experiment 1 qualification input.
It is generated from the existing Corpus v1 fixture, not used as a new corpus PDF:

```sh
pdftoppm -f 1 -singlefile -r 200 -png \
  corpus/fixtures/unit-order-01-natural/unit-order-01-natural.pdf \
  unit-order-01-natural-200dpi
```

Generation contract:

- source PDF: `unit-order-01-natural.pdf` from Corpus v1;
- renderer: Poppler `pdftoppm`, 200 DPI, RGB PNG, annotations unchanged;
- dimensions: 1167 x 612 pixels;
- SHA-256: `70e45c2a9c1f13636379315b257b7a684706df36d7dc7ba73e5293fe57f0effe`.

Independent acceptance used the source fixture's hand-written/adjudicated geometry and the
archived M0 PoC. With the pinned model, the raster produces six `text` boxes in query order
`23, 53, 125, 154, 230, 283`, matching `docs/04-m0-experiment-1.md`.
