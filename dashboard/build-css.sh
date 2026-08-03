#!/bin/bash
# Build dashboard CSS — concatenates modular CSS files in dependency order
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CSS_DIR="$SCRIPT_DIR/static/css"
OUT="$SCRIPT_DIR/static/style.css"

MODULES=(
    00-tokens.css
    01-reset.css
    02-typography.css
    03-scrollbar.css
    04-layout.css
    05-panels.css
    06-forms.css
    07-components.css
    08-modals.css
    09-messaging.css
    09-channels.css
    10-views.css
    11-games.css
    12-animations.css
    13-responsive.css
)

: > "$OUT"
for module in "${MODULES[@]}"; do
    cat "$CSS_DIR/$module" >> "$OUT"
    printf '\n' >> "$OUT"
done

# Keep this byte-for-byte equivalent to src-tauri/build.rs. A single output
# format prevents a shell rebuild and a Cargo rebuild from continuously
# replacing each other's bundle.
echo "Built $OUT ($(wc -l < "$OUT" | tr -d ' ') lines, $(wc -c < "$OUT" | tr -d ' ')B)"
