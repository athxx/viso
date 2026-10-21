#!/bin/sh
# Regenerate RobotoVF-subset.ttf (needs fontTools): a few Latin glyphs of the
# variable Roboto (SIL OFL 1.1; wght 100–900, wdth 75–100, ital 0–1), with
# every variation table kept.
pyftsubset "$1" --text="AHailnox " --output-file=RobotoVF-subset.ttf \
    --layout-features='*' --name-IDs='*' --notdef-outline
