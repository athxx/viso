#!/bin/sh
# Regenerate the editor tests' faces (needs fontTools) from DejaVu Sans Mono
# and DejaVu Sans (Bitstream Vera license with the DejaVu changes, free to
# redistribute): `make_subsets.sh DejaVuSansMono.ttf DejaVuSans.ttf`.
# The monospace face covers printable ASCII only; the letters past it come
# from the proportional face, in a fallback's widths.
pyftsubset "$1" --unicodes="U+0020-007E" --output-file=DejaVuSansMono-subset.ttf \
    --layout-features='*' --notdef-outline
pyftsubset "$2" --text="àéñößøćšłαλΩжяЩ─│┌┘┼" \
    --output-file=DejaVuSans-letters-subset.ttf --layout-features='*' --notdef-outline
