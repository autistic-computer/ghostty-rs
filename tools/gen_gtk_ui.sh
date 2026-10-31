#!/usr/bin/env bash
# Regenerates the GtkBuilder `.ui` files under `src/apprt/gtk/ui/` from their
# Blueprint (`.blp`) sources. The generated `.ui` files are committed so that
# building Ghostty never needs `blueprint-compiler`; `build.rs` bundles them
# (plus `src/apprt/gtk/css/` and `images/gnome/`) into a GResource.
#
# This replaces the `blueprint-compiler compile` steps of the Zig build
# (`gtkDistResources` in `src/build/SharedDeps.zig`) together with the version
# check in `src/apprt/gtk/build/blueprint.zig`.
#
# Ghostty requires blueprint-compiler 0.20.0 or newer. To use one that is not
# installed system-wide, e.g. a source checkout:
#
#   curl -L https://gitlab.gnome.org/jwestman/blueprint-compiler/-/archive/v0.20.4/blueprint-compiler-v0.20.4.tar.gz | tar xz
#   BLUEPRINT_COMPILER="python3 $PWD/blueprint-compiler-v0.20.4/blueprint-compiler.py" tools/gen_gtk_ui.sh
#
# (blueprint-compiler needs PyGObject, so pick a Python that has the `gi`
# module, e.g. `python3.12` on Ubuntu 24.04.)
set -euo pipefail

cd "$(dirname "$0")/.."
BLUEPRINT_COMPILER="${BLUEPRINT_COMPILER:-blueprint-compiler}"
REQUIRED="0.20.0"

version="$($BLUEPRINT_COMPILER --version | tr -d '[:space:]')"
case "$version" in
uninstalled) ;; # A source checkout run in place reports no version.
*)
    if [ "$(printf '%s\n%s\n' "$REQUIRED" "$version" | sort -V | head -n1)" != "$REQUIRED" ]; then
        echo "blueprint-compiler $version is too old; Ghostty requires $REQUIRED or newer" >&2
        exit 1
    fi
    ;;
esac

ui=src/apprt/gtk/ui
for blp in "$ui"/*/*.blp; do
    out="${blp%.blp}.ui"
    echo "compiling $blp -> $out"
    $BLUEPRINT_COMPILER compile --output "$out" "$blp"
done
