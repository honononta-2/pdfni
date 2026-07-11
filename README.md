# reticella

## Third-party notices

### pdf.js

The PDF reader (`src/reader/`) is ported in part from [Mozilla pdf.js](https://github.com/mozilla/pdf.js) and substantially modified for this library. The data tables in `src/reader/data/` (encodings, glyph list, standard font metrics, binary CMaps) are generated from pdf.js 6.0.227. pdf.js is licensed under the [Apache License 2.0](https://github.com/mozilla/pdf.js/blob/master/LICENSE).

### Adobe CMap resources

The bundled binary CMap data (`src/reader/data/bcmaps.rs`) is derived from [Adobe's CMap resources](https://github.com/adobe-type-tools/cmap-resources), obtained via pdf.js `external/bcmaps`. Copyright 1990-2019 Adobe. Licensed under the [BSD-3-Clause license](https://github.com/adobe-type-tools/cmap-resources/blob/master/LICENSE.md).
