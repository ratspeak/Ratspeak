#!/bin/bash
# Build dashboard CSS — concatenates modular CSS files in dependency order
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CSS_DIR="$SCRIPT_DIR/static/css"
OUT="$SCRIPT_DIR/static/style.css"

cat \
    "$CSS_DIR/00-tokens.css" \
    "$CSS_DIR/01-reset.css" \
    "$CSS_DIR/02-typography.css" \
    "$CSS_DIR/03-scrollbar.css" \
    "$CSS_DIR/04-layout.css" \
    "$CSS_DIR/05-panels.css" \
    "$CSS_DIR/06-forms.css" \
    "$CSS_DIR/07-components.css" \
    "$CSS_DIR/08-modals.css" \
    "$CSS_DIR/09-messaging.css" \
    "$CSS_DIR/10-views.css" \
    "$CSS_DIR/11-games.css" \
    "$CSS_DIR/12-animations.css" \
    "$CSS_DIR/13-responsive.css" \
    > "$OUT"

# Minify: strip comments, trim trailing whitespace, drop blank lines.
# All via perl — BSD and GNU `sed -i` syntax differ, and the BSD form
# silently no-ops on Linux.
UNMIN_SIZE=$(wc -c < "$OUT" | tr -d ' ')
perl -0777 -pi -e 's{/\*.*?\*/}{}gs; s/[ \t]+$//mg; s/\n+/\n/g; s/\A\n//' "$OUT"
MIN_SIZE=$(wc -c < "$OUT" | tr -d ' ')

echo "Built $OUT ($(wc -l < "$OUT" | tr -d ' ') lines, ${UNMIN_SIZE}B → ${MIN_SIZE}B)"
