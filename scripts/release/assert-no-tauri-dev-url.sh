#!/bin/bash
set -euo pipefail

PATTERN='TAURI_DEV_HOST|https?://((localhost|127\.0\.0\.1|0\.0\.0\.0):(1420|1430)|10\.[0-9.]+:[0-9]+|192\.168\.[0-9.]+:[0-9]+|172\.(1[6-9]|2[0-9]|3[0-1])\.[0-9.]+:[0-9]+)'
FAILED=0

if [ "$#" -eq 0 ]; then
    set -- \
        "src-tauri/target/release/bundle" \
        "src-tauri/gen/android/app/build/outputs" \
        "src-tauri/gen/apple/build"
fi

scan_file() {
    local file="$1"
    local label="$2"
    local matches

    matches="$(LC_ALL=C grep -aEo "$PATTERN" "$file" 2>/dev/null | sort -u | head -20 || true)"
    if [ -n "$matches" ]; then
        echo "ERROR: possible Tauri dev URL found in $label" >&2
        echo "$matches" >&2
        FAILED=1
    fi
}

scan_archive() {
    local archive="$1"
    local entry matches

    if ! command -v unzip >/dev/null 2>&1; then
        echo "WARNING: unzip is unavailable; cannot inspect $archive" >&2
        return
    fi

    while IFS= read -r entry; do
        case "$entry" in
            *.conf|*.html|*.js|*.json|*.plist|*.txt|*.xml|*tauri.conf.json*)
                matches="$(unzip -p "$archive" "$entry" 2>/dev/null | LC_ALL=C grep -aEo "$PATTERN" | sort -u | head -20 || true)"
                if [ -n "$matches" ]; then
                    echo "ERROR: possible Tauri dev URL found in $archive:$entry" >&2
                    echo "$matches" >&2
                    FAILED=1
                fi
                ;;
        esac
    done < <(unzip -Z1 "$archive" 2>/dev/null || true)
}

scan_target() {
    local target="$1"

    if [ ! -e "$target" ]; then
        echo "Skipping missing target: $target"
        return
    fi

    if [ -d "$target" ]; then
        while IFS= read -r -d '' file; do
            case "$file" in
                *.aab|*.apk|*.ipa|*.msix|*.zip)
                    scan_archive "$file"
                    ;;
            esac
            scan_file "$file" "$file"
        done < <(find "$target" -type f -print0)
    else
        case "$target" in
            *.aab|*.apk|*.ipa|*.msix|*.zip)
                scan_archive "$target"
                ;;
        esac
        scan_file "$target" "$target"
    fi
}

for target in "$@"; do
    scan_target "$target"
done

if [ "$FAILED" -ne 0 ]; then
    exit 1
fi

echo "No Tauri dev URL markers found."
