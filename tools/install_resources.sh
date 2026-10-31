#!/usr/bin/env bash
# Install Ghostty's runtime resources into <prefix>/share, mirroring
# `src/build/GhosttyResources.zig` (the Zig build's `zig-out/share`).
#
# usage: tools/install_resources.sh [prefix]   (default: target/dist)
#
# The macOS Xcode project bundles <repo>/target/dist/share/* into
# Ghostty.app, so run this before building the app.
#
# Env:
#   GHOSTTY_EMIT_WEBDATA=1 also generate the website reference (webdata).
#   GHOSTTY_DEBUG_APP=1    Linux desktop files with the debug app id.
#   GHOSTTY_INSTALLED_EXE_PATH  how the Linux desktop files launch ghostty.
#   GHOSTTY_EMIT_THEMES=0  skip downloading the iTerm2 color theme pack.
#   CARGO_BUILD_ARGS       extra args for building ghostty-build-data.
set -euo pipefail
cd "$(dirname "$0")/.."
prefix="${1:-target/dist}"
share="$prefix/share"
mkdir -p "$share"

# The build-data generator runs on the host, like Zig's `b.graph.host`.
# shellcheck disable=SC2086
cargo build -q --release --no-default-features --bin ghostty-build-data ${CARGO_BUILD_ARGS:-}
gen=target/release/ghostty-build-data

# Terminfo: source, termcap, and the compiled database.
mkdir -p "$share/terminfo"
"$gen" +terminfo > "$share/terminfo/ghostty.terminfo"
if command -v infotocap >/dev/null; then
  infotocap "$share/terminfo/ghostty.terminfo" > "$share/terminfo/ghostty.termcap" 2>/dev/null
fi
if command -v tic >/dev/null; then
  tic -x -o "$share/terminfo" "$share/terminfo/ghostty.terminfo" 2>/dev/null
else
  echo "warning: tic not found; terminfo database not compiled" >&2
fi

# Shell integration (everything but the READMEs).
rm -rf "$share/ghostty/shell-integration"
mkdir -p "$share/ghostty"
cp -R src/shell-integration "$share/ghostty/shell-integration"
find "$share/ghostty/shell-integration" -name '*.md' -delete

# Themes (`iterm2_themes` in ghostty's build.zig.zon).
if [ "${GHOSTTY_EMIT_THEMES:-1}" != 0 ] && [ ! -d "$share/ghostty/themes" ]; then
  url="https://deps.files.ghostty.org/ghostty-themes-release-20261019-151001-86a776d.tgz"
  tmp=$(mktemp -d)
  if curl -fsSL "$url" -o "$tmp/themes.tgz"; then
    mkdir -p "$share/ghostty/themes"
    # Like Zig's package fetch, drop the archive's top-level directory.
    tar -xzf "$tmp/themes.tgz" -C "$share/ghostty/themes" --strip-components=1
    find "$share/ghostty/themes" -name '*.md' -delete
  else
    echo "warning: could not download themes from $url" >&2
  fi
  rm -rf "$tmp"
fi

# Shell completions.
mkdir -p "$share/fish/vendor_completions.d" "$share/zsh/site-functions" \
  "$share/bash-completion/completions"
"$gen" +fish > "$share/fish/vendor_completions.d/ghostty.fish"
"$gen" +zsh > "$share/zsh/site-functions/_ghostty"
"$gen" +bash > "$share/bash-completion/completions/ghostty.bash"

# Vim and Neovim plugin.
for dir in "$share/vim/vimfiles" "$share/nvim/site"; do
  mkdir -p "$dir"/{syntax,ftdetect,ftplugin,compiler}
  "$gen" +vim-syntax > "$dir/syntax/ghostty.vim"
  "$gen" +vim-ftdetect > "$dir/ftdetect/ghostty.vim"
  "$gen" +vim-ftplugin > "$dir/ftplugin/ghostty.vim"
  "$gen" +vim-compiler > "$dir/compiler/ghostty.vim"
done

# Sublime syntax for bat.
mkdir -p "$share/bat/syntaxes"
"$gen" +sublime > "$share/bat/syntaxes/ghostty.sublime-syntax"

# Translations (`src/build/GhosttyI18n.zig`): po/<locale>.po ->
# share/locale/<locale>/LC_MESSAGES/com.mitchellh.ghostty.mo.
mkdir -p "$share/locale"
if command -v msgfmt >/dev/null; then
  for po in po/*.po; do
    locale=$(basename "$po" .po)
    mkdir -p "$share/locale/$locale/LC_MESSAGES"
    msgfmt -o "$share/locale/$locale/LC_MESSAGES/com.mitchellh.ghostty.mo" "$po"
  done
else
  echo "warning: msgfmt not found; translations not compiled" >&2
fi

# Man pages and HTML docs (`src/build/GhosttyDocs.zig`): mdgen markdown,
# converted with pandoc when it is installed. The Xcode project expects
# share/man to exist either way.
mkdir -p "$share/man" "$share/ghostty/doc"
# shellcheck disable=SC2086
cargo build -q --release --no-default-features --features docs \
  --bin mdgen_ghostty_1 --bin mdgen_ghostty_5 ${CARGO_BUILD_ARGS:-}
for page in ghostty:1 ghostty:5; do
  name=${page%:*}; section=${page#*:}
  md="$share/ghostty/doc/$name.$section.md"
  "target/release/mdgen_${name}_$section" > "$md"
  if command -v pandoc >/dev/null; then
    pandoc --standalone --from markdown --to html "$md" > "$share/ghostty/doc/$name.$section.html"
    mkdir -p "$share/man/man$section"
    pandoc --standalone --from markdown --to man "$md" > "$share/man/man$section/$name.$section"
  fi
done
command -v pandoc >/dev/null || echo "warning: pandoc not found; man pages not generated" >&2

# Website data (`src/build/GhosttyWebdata.zig`, Zig: -Demit-webdata), off by
# default as in Zig.
if [ "${GHOSTTY_EMIT_WEBDATA:-0}" = 1 ]; then
  # shellcheck disable=SC2086
  cargo build -q --release --no-default-features --features docs \
    --bin webgen_config --bin webgen_actions --bin webgen_commands ${CARGO_BUILD_ARGS:-}
  mkdir -p "$share/ghostty/webdata"
  for page in config actions commands; do
    "target/release/webgen_$page" > "$share/ghostty/webdata/$page.mdx"
  done
fi

# Linux desktop integration (`addLinuxAppResources`).
if [ "$(uname -s)" = Linux ]; then
  # Release builds; set GHOSTTY_DEBUG_APP=1 for the debug app id and name.
  if [ "${GHOSTTY_DEBUG_APP:-0}" = 1 ]; then
    name="Ghostty (Debug)"; app_id=com.mitchellh.ghostty-debug
  else
    name=Ghostty; app_id=com.mitchellh.ghostty
  fi
  # How the files launch Ghostty (Zig: -Dinstalled-exe-path).
  exe="${GHOSTTY_INSTALLED_EXE_PATH:-ghostty}"

  # The templates are cmake-style (@VAR@); Zig's config header output has
  # a header line that it strips, which plain substitution doesn't add.
  template() {
    mkdir -p "$(dirname "$2")"
    sed -e "s|@NAME@|$name|g" -e "s|@APPID@|$app_id|g" -e "s|@GHOSTTY@|$exe|g" "$1" > "$2"
  }
  template dist/linux/app.desktop.in "$share/applications/$app_id.desktop"
  template dist/linux/dbus.service.in "$share/dbus-1/services/$app_id.service"
  template dist/linux/systemd.service.in "$share/systemd/user/app-$app_id.service"
  template dist/linux/com.mitchellh.ghostty.metainfo.xml.in "$share/metainfo/$app_id.metainfo.xml"

  # Right click menu actions for Plasma (Dolphin) and Nautilus.
  mkdir -p "$share/kio/servicemenus" "$share/nautilus-python/extensions"
  cp dist/linux/ghostty_dolphin.desktop "$share/kio/servicemenus/com.mitchellh.ghostty.desktop"
  cp dist/linux/ghostty_nautilus.py "$share/nautilus-python/extensions/ghostty.py"

  # Icons.
  icon() {
    mkdir -p "$share/icons/hicolor/$2/apps"
    cp "images/gnome/$1.png" "$share/icons/hicolor/$2/apps/com.mitchellh.ghostty.png"
  }
  icon 16 16x16; icon 32 32x32; icon 128 128x128; icon 256 256x256
  icon 512 512x512; icon 1024 1024x1024
  icon 32 16x16@2; icon 64 32x32@2; icon 256 128x128@2; icon 512 256x256@2
fi

echo "resources installed to $share"
