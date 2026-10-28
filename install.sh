#!/bin/sh
# NyForge installer for Linux (Windows: install.cmd). Works from the release
# tarball (binaries next to this script) and from a git clone (builds with cargo first).
# Installs the `nyforge` command and the `nyforge-gui` desktop app.
#
# Run it with sudo (it needs to write to /usr/local):
#
#   sudo ./install.sh                 install to /usr/local
#   sudo ./install.sh --prefix /opt/nyforge
#   sudo ./install.sh --uninstall
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
prefix=/usr/local
uninstall=0

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix=$2; shift ;;
    --prefix=*) prefix=${1#--prefix=} ;;
    --uninstall) uninstall=1 ;;
    -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

if [ "$(id -u)" != 0 ]; then
  echo "NyForge has to be installed with sudo. Run:" >&2
  if [ "$uninstall" = 1 ]; then echo "  sudo ./install.sh --uninstall" >&2; else echo "  sudo ./install.sh" >&2; fi
  exit 1
fi
bin=$prefix/bin

if [ "$uninstall" = 1 ]; then
  rm -f "$bin/nyforge" "$bin/nyforge-gui" "$bin/nyforge-uninstall"
  rm -rf "$prefix/share/doc/nyforge"
  rm -f "$prefix/share/applications/nyforge.desktop" \
        "$prefix/share/icons/hicolor/256x256/apps/nyforge.png" \
        "$prefix/share/icons/hicolor/scalable/apps/nyforge.svg" \
        /usr/share/polkit-1/actions/org.nyforge.engine.policy 2>/dev/null || true
  echo "NyForge removed from $prefix."
  exit 0
fi

# Find or build the binary.
if [ -x "$here/bin/nyforge" ]; then
  src=$here/bin
  assets=$here
elif [ -f "$here/Cargo.toml" ]; then
  echo "Building NyForge (this takes a few minutes the first time)..."
  if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    # Started with sudo: root has no Rust set up, but the person who typed sudo does. Build as that person
    # (so the build folder stays theirs) and only install as root.
    uhome=$(getent passwd "$SUDO_USER" | cut -d: -f6)
    [ -x "$uhome/.cargo/bin/cargo" ] || command -v cargo >/dev/null 2>&1 || { echo "Building from source needs Rust (https://rustup.rs). Or download a release tarball." >&2; exit 1; }
    (cd "$here" && sudo -u "$SUDO_USER" -H env "PATH=$uhome/.cargo/bin:$PATH" cargo build --release --locked -p nyforge -p nyforge-gui)
  else
    command -v cargo >/dev/null 2>&1 || { echo "Building from source needs Rust (https://rustup.rs). Or download a release tarball." >&2; exit 1; }
    (cd "$here" && cargo build --release --locked -p nyforge -p nyforge-gui)
  fi
  src=$here/target/release
  assets=$here
else
  echo "Cannot find the NyForge binary or source next to install.sh." >&2
  exit 1
fi

if ! mkdir -p "$bin" "$prefix/share/doc/nyforge" 2>/dev/null; then
  echo "Cannot write to $prefix. Run the installer with sudo:  sudo ./install.sh" >&2
  exit 1
fi
install -m 0755 "$src/nyforge" "$bin/nyforge"
have_gui=0
if [ -x "$src/nyforge-gui" ]; then
  install -m 0755 "$src/nyforge-gui" "$bin/nyforge-gui"
  have_gui=1
fi
for f in LICENSE README.md THIRD-PARTY.md; do
  [ -f "$here/$f" ] && install -m 0644 "$here/$f" "$prefix/share/doc/nyforge/$f"
done

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
fi

echo "NyForge installed in $bin"
case ":$PATH:" in *":$bin:"*) ;; *) echo "Note: $bin is not on your PATH." ;; esac
if [ "$have_gui" = 1 ]; then
  echo "Start the app from your applications menu, or run:  nyforge-gui"
fi
echo "Command line:  nyforge list"
echo "To remove NyForge later:  nyforge-uninstall"
