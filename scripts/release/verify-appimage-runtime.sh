#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "usage: $0 <appimage-path|extracted-appdir>" >&2
  exit 2
fi

target="$(realpath "$1")"
extract_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$extract_dir"
}
trap cleanup EXIT

if [[ -d "$target" ]]; then
  appdir="$target"
elif [[ -f "$target" && -x "$target" ]]; then
  # --appimage-extract creates every directory 0700; unsquashfs under umask 0 keeps stored modes.
  offset="$("$target" --appimage-offset)"
  (
    umask 0000
    unsquashfs -no-progress -no-xattrs -offset "$offset" -dest "$extract_dir/squashfs-root" "$target" >/dev/null
  )
  appdir="$extract_dir/squashfs-root"
else
  echo "AppImage is missing or not executable: $target" >&2
  exit 1
fi

status=0
fail() {
  echo "$1" >&2
  status=1
}
report() {
  local message="$1"
  local found="$2"
  if [[ -n "$found" ]]; then
    fail "$message"
    printf '%s\n' "$found" | head -n 20 >&2
  fi
}

runtime_dir="$appdir/usr/lib"
sonames=(
  libayatana-appindicator3.so.1
  libayatana-ido3-0.4.so.0
  libayatana-indicator3.so.7
  libdbusmenu-glib.so.4
  libdbusmenu-gtk3.so.4
)

for soname in "${sonames[@]}"; do
  library="$runtime_dir/$soname"
  if [[ ! -s "$library" ]]; then
    echo "AppImage is missing required indicator runtime library: $soname" >&2
    exit 1
  fi
  if ! LC_ALL=C file "$library" | grep -Eq 'ELF 64-bit LSB .*x86-64'; then
    echo "AppImage contains an invalid indicator runtime library: $soname" >&2
    exit 1
  fi
done

# Tray libraries are dlopened, so linuxdeploy never proves their closure.
if [[ "$(uname -s)" == "Linux" ]]; then
  dependency_report="$(LD_LIBRARY_PATH="$runtime_dir" ldd "$runtime_dir/libayatana-appindicator3.so.1")"
  for dependency in "${sonames[@]:1}"; do
    if ! grep -F "$dependency => $runtime_dir/$dependency" <<<"$dependency_report" >/dev/null; then
      fail "AppImage indicator dependency escaped its bundled runtime: $dependency"
      printf '%s\n' "$dependency_report" >&2
    fi
  done
else
  echo "Skipping indicator closure check: ldd needs Linux" >&2
fi

# Bundled libwayland breaks EGL against newer host Mesa; the host copy must win.
report "AppImage bundles libwayland, which must come from the host:" \
  "$(find "$appdir" -name 'libwayland-*' \( -type f -o -type l \))"

# Forcing X11 leaves Wayland sessions without Xwayland unable to open a display.
hooks=()
for hook in "$appdir"/apprun-hooks/*.sh; do
  [[ -f "$hook" ]] && hooks+=("$hook")
done
if [[ "${#hooks[@]}" -gt 0 ]]; then
  report "AppRun hooks must not set GDK_BACKEND:" \
    "$(awk '{ line = $0; sub(/#.*/, "", line) } line ~ /GDK_BACKEND/ { print FILENAME ":" FNR ": " $0 }' "${hooks[@]}")"
fi

# The mounting user is not always the running user (firejail --appimage, catalog checks).
for launcher in AppRun AppRun.wrapped; do
  if [[ ! -f "$appdir/$launcher" ]]; then
    fail "AppImage is missing $launcher"
  elif [[ -z "$(find "$appdir/$launcher" -prune -perm 0755)" ]]; then
    fail "AppImage $launcher must be mode 0755: $(ls -ld "$appdir/$launcher")"
  fi
done
report "AppImage files must be world-readable:" \
  "$(find "$appdir" -type f ! -perm -0444)"
report "AppImage executables must be world-executable:" \
  "$(find "$appdir" -type f -perm -0100 ! -perm -0001)"
report "AppImage directories must be world-traversable:" \
  "$(find "$appdir" -type d ! -perm -0555)"

# WebKit media: <audio> WAV voice memos, Web Audio ringtones, and camera capture.
gst_dir="$runtime_dir/gstreamer-1.0"
gst_plugins=(
  libgstcoreelements.so
  libgstapp.so
  libgstplayback.so
  libgsttypefindfunctions.so
  libgstaudioconvert.so
  libgstaudioresample.so
  libgstvolume.so
  libgstwavparse.so
  libgstautodetect.so
  libgstpulseaudio.so
  libgstalsa.so
  libgstvideo4linux2.so
  libgstvideoconvert.so
  libgstvideoscale.so
  libgstjpeg.so
  libgstopengl.so
)
if [[ ! -d "$gst_dir" ]]; then
  fail "AppImage is missing its GStreamer plugin directory: usr/lib/gstreamer-1.0"
else
  for plugin in "${gst_plugins[@]}"; do
    [[ -s "$gst_dir/$plugin" ]] || fail "AppImage is missing GStreamer plugin: $plugin"
  done
fi
[[ -x "$runtime_dir/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner" ]] ||
  fail "AppImage is missing gst-plugin-scanner"
gst_hook="$appdir/apprun-hooks/linuxdeploy-plugin-gstreamer.sh"
# shellcheck disable=SC2016
if [[ ! -f "$gst_hook" ]] || ! grep -Fq 'GST_PLUGIN_SYSTEM_PATH_1_0="${APPDIR}/usr/lib/gstreamer-1.0"' "$gst_hook"; then
  fail "AppImage GStreamer hook does not confine plugins to the AppImage"
fi

if [[ "$status" -ne 0 ]]; then
  exit "$status"
fi
echo "AppImage runtime verified"
