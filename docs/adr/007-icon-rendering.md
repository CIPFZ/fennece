# ADR-007: Icon rendering without an SVG library

- Status: accepted (M0)
- Related: SPEC ch.0 §1.2, AGENT M0 task 8

## Background
The icon must exist as SVG and as `.ico` (16–256) plus monochrome tray PNGs. A generic SVG rasteriser
(`resvg`) is > 10 k lines and not in the dependency list.

## Decision
`tools/gen-icons` defines the geometry once (rounded rect, gradient, ring, ear strokes, capsule handle,
discs, triangle, arc), emits `fennec.svg` from it and rasterises the same shapes with signed-distance tests
and 4×4 supersampling. ICO files embed PNG images (supported since Windows Vista). Strokes are thickened
for ≤ 32 px so lines stay legible.

## Consequences
SVG and bitmaps cannot drift; only `png` is needed. Complex SVG features are out of scope — edit the
geometry in the tool, not the generated SVG.
