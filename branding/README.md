# AVA1 brand

The AVA1 mark is one zigzag stroke, /\/\, which reads Λ V Λ: the three letters of AVA share
their strokes, and a crossbar under each peak makes the two A's. It also reads as a path over
the network. It is drawn in the same ink (`#141416`) and rounded weight as the
[ps5upload](https://github.com/phantomptr/ps5upload) mark, so the two read as a family. The name
is set in [Titan One](https://fonts.google.com/specimen/Titan+One) (SIL Open Font License,
[`fonts/TitanOne-OFL.txt`](fonts/TitanOne-OFL.txt)) and kept as outlines, so no font is needed to
use the files.

## Masters (SVG)

| File | Use |
| --- | --- |
| `icon.svg` | App icon: black mark on a white rounded tile |
| `icon-dark.svg` | White mark on a black rounded tile |
| `mark.svg`, `mark-white.svg` | The mark alone, transparent |
| `lockup-horizontal-black.svg`, `-white.svg` | Mark beside the name, transparent |
| `lockup-stacked-black.svg`, `-white.svg` | Mark above the name, transparent |

Use the black files on light backgrounds and the white files on dark ones.

## Images (`png/`)

The masters at full size, plus `icon-256.png` and `favicon-32.png` (the logo and favicon on
docs.rs) and `github-social-preview.png` (1280×640, for the repository's social preview).

## Changing the logo

Edit the SVG masters, then run:

```sh
python3 scripts/brand-assets.py    # needs rsvg-convert (brew install librsvg) and Pillow
```
