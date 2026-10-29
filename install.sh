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
#   --uninstall       remove NyForge
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
prefix=/usr/local
uninstall=0
run=0
yes=0
want_tui=1
want_gui=1

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix=$2; shift ;;
    --prefix=*) prefix=${1#--prefix=} ;;
    --uninstall) uninstall=1 ;;
    --run) run=1 ;;      # used by the installer screen
    --tui) want_tui=$2; shift ;;
    --gui) want_gui=$2; shift ;;
    -y|--yes) yes=1 ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
  say "NyForge removed from $prefix"
  exit 0
fi

# Find or build the programs.
if [ -x "$here/bin/nyforge" ]; then
  src=$here/bin
  assets=$here
elif [ "$run" = 1 ] && [ -x "$here/target/release/nyforge" ]; then
  src=$here/target/release
  assets=$here
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

# Ask first, in the installer screen, when there is a terminal to ask in.
if [ "$run" = 0 ] && [ "$yes" = 0 ] && [ -t 0 ] && [ -t 1 ] && [ -x "$src/nyforge-tui" ]; then
  rc=0
  "$src/nyforge-tui" --installer --script "$here/install.sh" --prefix "$prefix" --gui "$have_gui" || rc=$?
  case "$rc" in
    0) exit 0 ;;
    3) echo "Nothing was installed."; exit 0 ;;
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
say "Copied the programs"

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
  echo "Terminal app:  nyforge -t"
  [ "$have_gui" = 1 ] && echo "Desktop app:   nyforge-gui"
  echo "Command line:  nyforge list"
  echo "Remove it:     sudo nyforge-uninstall"
fi
