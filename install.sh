#!/bin/sh
# NyForge installer for Linux (Windows: install.cmd). Works from the release tarball (binaries next to this
# script) and from a git clone (builds with cargo first).
# Installs the `nyforge` command, the `nyforge-tui` terminal app (`nyforge -t`) and the `nyforge-gui` desktop app.
#
# The one way to install it:
#
#   sudo ./install.sh
#
# Other options (also with sudo ./install.sh):
#   --prefix DIR      install under DIR instead of /usr/local
#   --yes             do not ask anything: install everything
#   --build           build from source even when a download exists
#   --update          install the newest release over the installed one
#   --uninstall       remove NyForge
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
prefix=/usr/local
uninstall=0
update=0
run=0
yes=0
want_tui=1
want_gui=1
force_build=0
src_arg=""
assets_arg=""

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix=$2; shift ;;
    --prefix=*) prefix=${1#--prefix=} ;;
    --uninstall) uninstall=1 ;;
    --update) update=1 ;;
    --run) run=1 ;;      # used by the installer screen
    --tui) want_tui=$2; shift ;;
    --gui) want_gui=$2; shift ;;
    --src) src_arg=$2; shift ;;       # used by the installer screen
    --assets) assets_arg=$2; shift ;;
    --build) force_build=1 ;;
    -y|--yes) yes=1 ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

# Only `sudo ./install.sh` installs NyForge. `sh install.sh`, `bash install.sh`, `./install.sh` without sudo and
# the like stop here. sudo records exactly what it was asked to run, so that is what is checked.
first=${SUDO_COMMAND:-}
first=${first%% *}
if [ -z "${SUDO_COMMAND:-}" ] || ! { [ "$first" = "$here/install.sh" ] || { [ -f "$first" ] && [ "$first" -ef "$here/install.sh" ]; }; }; then
  echo "To install the program run: sudo ./install.sh" >&2
  exit 1
fi
if [ "$(id -u)" != 0 ]; then
  echo "To install the program run: sudo ./install.sh" >&2
  exit 1
fi
bin=$prefix/bin

if [ -t 1 ] && [ "$run" = 0 ]; then
  g=$(printf '\033[32m') d=$(printf '\033[2m') r=$(printf '\033[0m')
else
  g="" d="" r=""
fi
# One line per finished step. The installer screen reads the `step` lines.
say() { if [ "$run" = 1 ]; then printf 'step %s\n' "$1"; else printf '%s✔%s %s\n' "$g" "$r" "$1"; fi; }
note() { if [ "$run" = 1 ]; then printf 'note %s\n' "$1"; else printf '%s%s%s\n' "$d" "$1" "$r"; fi; }

if [ "$uninstall" = 1 ]; then
  rm -f "$bin/nyforge" "$bin/nyforge-gui" "$bin/nyforge-tui" "$bin/nyforge-uninstall"
  rm -rf "$prefix/share/doc/nyforge"
  rm -f "$prefix/share/applications/nyforge.desktop" \
        "$prefix/share/icons/hicolor/256x256/apps/nyforge.png" \
        "$prefix/share/icons/hicolor/scalable/apps/nyforge.svg" \
        /usr/share/polkit-1/actions/org.nyforge.engine.policy 2>/dev/null || true
  say "Removed the programs"
  say "Removed the menu entry and icons"
  say "Removed the documentation"
  exit 0
fi

# Downloads release $1 (x86_64 Linux) into a temporary folder, checks it against its SHA-256 and sets $src and $assets.
download_release() {
  [ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || return 1
  command -v curl >/dev/null 2>&1 && command -v sha256sum >/dev/null 2>&1 || return 1
  name=nyforge-$1-linux-x86_64
  url=https://github.com/77nyan/nyforge/releases/download/v$1
  dl=$(mktemp -d) || return 1
  trap 'rm -rf "$dl"' EXIT
  echo "Downloading NyForge $1..."
  curl -fsL --retry 2 --connect-timeout 10 -o "$dl/$name.tar.gz" "$url/$name.tar.gz" || return 1
  curl -fsL --retry 2 --connect-timeout 10 -o "$dl/$name.tar.gz.sha256" "$url/$name.tar.gz.sha256" || return 1
  (cd "$dl" && sha256sum -c "$name.tar.gz.sha256" >/dev/null 2>&1) || { echo "The download did not match its checksum." >&2; return 1; }
  tar -xzf "$dl/$name.tar.gz" -C "$dl" || return 1
  [ -x "$dl/$name/bin/nyforge" ] || return 1
  src=$dl/$name/bin
  assets=$dl/$name
}

# The newest released version number, from GitHub: the page behind the "latest release" link, or, failing that,
# the newest version tag.
latest_version() {
  v=""
  if command -v curl >/dev/null 2>&1; then
    u=$(curl -sL --connect-timeout 10 -o /dev/null -w '%{http_code} %{url_effective}' https://github.com/77nyan/nyforge/releases/latest 2>/dev/null || true)
    case "$u" in 200\ */releases/tag/v*) v=${u##*/v} ;; esac
  fi
  if [ -z "$v" ] && command -v git >/dev/null 2>&1; then
    v=$(git ls-remote --tags --refs https://github.com/77nyan/nyforge 2>/dev/null | sed -n 's#.*refs/tags/v\([0-9][0-9.]*\)$#\1#p' | sort -V | tail -1)
  fi
  case "$v" in ""|*[!0-9.]*) return 1 ;; esac
  echo "$v"
}

# A clone at a released version installs that release's programs from GitHub instead of compiling them
# (about five minutes). Anything else, such as a changed clone, an unreleased version or no network, is built.
fetch_release() {
  ver=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
  [ -n "$ver" ] || return 1
  if [ -e "$here/.git" ]; then
    command -v git >/dev/null 2>&1 || return 1
    [ "$(git -c 'safe.directory=*' -C "$here" describe --tags --exact-match 2>/dev/null)" = "v$ver" ] || return 1
    [ -z "$(git -c 'safe.directory=*' -C "$here" status --porcelain 2>/dev/null)" ] || return 1
  fi
  download_release "$ver"
}

# Update: install the newest release over the installed one, keeping the same parts.
if [ "$update" = 1 ]; then
  cur=$("$bin/nyforge" --version 2>/dev/null | awk 'NR==1 {print $2}')
  if [ -z "$cur" ]; then
    echo "NyForge is not installed in $prefix. Install it first: sudo ./install.sh" >&2
    exit 1
  fi
  latest=$(latest_version) || { echo "Could not reach GitHub to look for a newer version." >&2; exit 1; }
  say "Installed $cur, newest $latest"
  if [ "$(printf '%s\n%s\n' "$cur" "$latest" | sort -V | tail -1)" = "$cur" ]; then
    say "NyForge is up to date"
    exit 0
  fi
  [ -x "$bin/nyforge-tui" ] || want_tui=0
  [ -x "$bin/nyforge-gui" ] || want_gui=0
  download_release "$latest" || { echo "Could not download NyForge $latest. Try again, or use a newer folder with: sudo ./install.sh" >&2; exit 1; }
  src_arg=$src
  assets_arg=$assets
fi

# Find or build the programs.
if [ -n "$src_arg" ]; then
  src=$src_arg
  assets=${assets_arg:-$here}
elif [ -x "$here/bin/nyforge" ]; then
  src=$here/bin
  assets=$here
elif [ "$run" = 1 ] && [ -x "$here/target/release/nyforge" ]; then
  src=$here/target/release
  assets=$here
elif [ "$force_build" = 0 ] && [ -f "$here/Cargo.toml" ] && fetch_release; then
  :
elif [ -f "$here/Cargo.toml" ]; then
  echo "Building NyForge (this takes a few minutes the first time)..."
  if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    # Started with sudo: root has no Rust set up, but the person who typed sudo does. Build as that person
    # (so the build folder stays theirs) and only install as root.
    uhome=$(getent passwd "$SUDO_USER" | cut -d: -f6)
    [ -x "$uhome/.cargo/bin/cargo" ] || command -v cargo >/dev/null 2>&1 || { echo "Building from source needs Rust (https://rustup.rs). Or download a release tarball." >&2; exit 1; }
    (cd "$here" && sudo -u "$SUDO_USER" -H env "PATH=$uhome/.cargo/bin:$PATH" cargo build --release --locked -p nyforge -p nyforge-gui -p nyforge-tui)
  else
    command -v cargo >/dev/null 2>&1 || { echo "Building from source needs Rust (https://rustup.rs). Or download a release tarball." >&2; exit 1; }
    (cd "$here" && cargo build --release --locked -p nyforge -p nyforge-gui -p nyforge-tui)
  fi
  src=$here/target/release
  assets=$here
else
  echo "Cannot find the NyForge binary or source next to install.sh." >&2
  exit 1
fi

have_gui=0
[ -x "$src/nyforge-gui" ] && have_gui=1
# The version shown is read from the program being installed, so it is always the one that goes onto the computer.
version=$("$src/nyforge" --version 2>/dev/null | awk 'NR==1 {print $2}')
[ -n "$version" ] || version=unknown

# Ask first, in the installer screen, when there is a terminal to ask in.
if [ "$run" = 0 ] && [ "$yes" = 0 ] && [ "$update" = 0 ] && [ -t 0 ] && [ -t 1 ] && [ -x "$src/nyforge-tui" ]; then
  rc=0
  "$src/nyforge-tui" --installer --script "$here/install.sh" --prefix "$prefix" --gui "$have_gui" --src "$src" --assets "$assets" || rc=$?
  case "$rc" in
    0) exit 0 ;;
    3) echo "Nothing was changed."; exit 0 ;;
    *) exit 1 ;;
  esac
fi
[ "$want_gui" = 1 ] || have_gui=0

if ! mkdir -p "$bin" "$prefix/share/doc/nyforge" 2>/dev/null; then
  echo "Cannot write to $prefix. Run the installer with: sudo ./install.sh" >&2
  exit 1
fi
install -m 0755 "$src/nyforge" "$bin/nyforge"
if [ "$want_tui" = 1 ] && [ -x "$src/nyforge-tui" ]; then
  install -m 0755 "$src/nyforge-tui" "$bin/nyforge-tui"
else
  want_tui=0
fi
if [ "$have_gui" = 1 ]; then
  install -m 0755 "$src/nyforge-gui" "$bin/nyforge-gui"
fi
for f in LICENSE README.md THIRD-PARTY.md; do
  if [ -f "$here/$f" ]; then install -m 0644 "$here/$f" "$prefix/share/doc/nyforge/$f"; fi
done
say "Copied the programs (NyForge $version)"

# Write the uninstaller next to the program, so it can be removed later without this folder.
uninst="$bin/nyforge-uninstall"
cat > "$uninst" <<'UNINSTALL_EOF'
#!/bin/sh
# NyForge uninstaller (written by install.sh). Removes NyForge from this computer: the programs, the menu entry, the
# icons and the documentation. It never touches your own files or any drive.
#   sudo nyforge-uninstall        asks first
#   sudo nyforge-uninstall -y     does not ask
set -u
yes=0
[ "${1:-}" = "-y" ] && yes=1
mine="@PREFIX@"
if [ "$(id -u)" != 0 ]; then
  echo "Run it with sudo:  sudo nyforge-uninstall" >&2
  exit 1
fi
prefixes="$mine /usr/local"
if [ "$yes" = 0 ]; then
  printf 'Remove NyForge from this computer? [y/N] '
  read -r a || a=n
  case "$a" in y|Y|yes|YES) ;; *) echo "Nothing was removed."; exit 0 ;; esac
fi
pkill -x nyforge-gui 2>/dev/null || true
failed=0
removed=0
rm_path() {
  [ -e "$1" ] || [ -L "$1" ] || return 0
  if rm -rf "$1" 2>/dev/null; then
    echo "removed $1"
    removed=1
  else
    echo "could not remove $1 (run it with sudo)" >&2
    failed=1
  fi
}
seen=" "
for p in $prefixes; do
  case "$seen" in *" $p "*) continue ;; esac
  seen="$seen$p "
  rm_path "$p/bin/nyforge"
  rm_path "$p/bin/nyforge-gui"
  rm_path "$p/bin/nyforge-tui"
  rm_path "$p/share/doc/nyforge"
  rm_path "$p/share/applications/nyforge.desktop"
  rm_path "$p/share/icons/hicolor/256x256/apps/nyforge.png"
  rm_path "$p/share/icons/hicolor/scalable/apps/nyforge.svg"
  [ -d "$p/share/applications" ] && command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$p/share/applications" >/dev/null 2>&1
done
rm_path /usr/share/polkit-1/actions/org.nyforge.engine.policy
# Last, this script itself (a running shell script can be deleted safely).
for p in $prefixes; do rm_path "$p/bin/nyforge-uninstall"; done
if [ "$failed" = 0 ]; then
  [ "$removed" = 1 ] && echo "NyForge has been removed." || echo "NyForge was not found."
else
  echo "Some parts are still installed. Run it again with sudo." >&2
  exit 1
fi
UNINSTALL_EOF
sed -i "s#@PREFIX@#$prefix#g" "$uninst"
chmod 0755 "$uninst"


say "Wrote nyforge-uninstall"
# Desktop integration (Linux): application menu entry, icon, and the polkit text shown when the app
# asks for permission to write to a drive. The app works without any of these.
if [ "$have_gui" = 1 ] && [ "$(uname -s)" = Linux ]; then
  if [ -f "$assets/packaging/nyforge.desktop.in" ]; then
    mkdir -p "$prefix/share/applications"
    sed "s#@BIN@#$bin#g" "$assets/packaging/nyforge.desktop.in" > "$prefix/share/applications/nyforge.desktop"
    chmod 0644 "$prefix/share/applications/nyforge.desktop"
  fi
  if [ -f "$assets/assets/logo/nyforge-256.png" ]; then
    mkdir -p "$prefix/share/icons/hicolor/256x256/apps" "$prefix/share/icons/hicolor/scalable/apps"
    install -m 0644 "$assets/assets/logo/nyforge-256.png" "$prefix/share/icons/hicolor/256x256/apps/nyforge.png"
    install -m 0644 "$assets/assets/logo/nyforge.svg" "$prefix/share/icons/hicolor/scalable/apps/nyforge.svg"
  fi
  # polkit only reads /usr/share/polkit-1/actions, so this is done only when we can write there.
  if [ -f "$assets/packaging/org.nyforge.engine.policy.in" ] && [ -w /usr/share/polkit-1/actions ] 2>/dev/null; then
    sed "s#@BIN@#$bin#g" "$assets/packaging/org.nyforge.engine.policy.in" > /usr/share/polkit-1/actions/org.nyforge.engine.policy
    chmod 0644 /usr/share/polkit-1/actions/org.nyforge.engine.policy
  fi
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
  command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q "$prefix/share/icons/hicolor" >/dev/null 2>&1 || true
  say "Added the menu entry and icon"
fi

case ":$PATH:" in *":$bin:"*) ;; *) note "$bin is not on your PATH." ;; esac
if [ "$run" = 0 ]; then
  echo
  echo "NyForge $version is installed in $bin"
  echo "Terminal app:  nyforge -t"
  [ "$have_gui" = 1 ] && echo "Desktop app:   nyforge-gui"
  echo "Command line:  nyforge list"
  echo "Remove it:     sudo nyforge-uninstall"
fi
