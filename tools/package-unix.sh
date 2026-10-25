#!/bin/sh
# Build a release archive for Linux or macOS from the current tree.
#
#   tools/package-unix.sh                                  native build for this machine: the desktop app
#                                                          (nyforge-gui) and the command line (nyforge)
#   tools/package-unix.sh --macos-universal               desktop app + command line for Apple Silicon and
#                                                          Intel Macs in one build, with NyForge.app (run on a Mac)
#   tools/package-unix.sh --target x86_64-unknown-linux-gnu.2.17
#                                                          command-line only, portable Linux build that runs on
#                                                          glibc 2.17+; needs `pip install ziglang cargo-zigbuild`
#   tools/package-unix.sh --target aarch64-unknown-linux-musl
#                                                          command-line only, fully static (also runs on Alpine)
#
# Output: dist/nyforge-<version>-<os>-<arch>.tar.gz (command-line-only builds: dist/nyforge-cli-...)
# and a .sha256 next to it.
set -eu
cd "$(dirname "$0")/.."

target=""
universal=0
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target=$2; shift ;;
    --target=*) target=${1#--target=} ;;
    --macos-universal) universal=1 ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "package-unix.sh: unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

norm_arch() { case "$1" in arm64|aarch64) echo aarch64 ;; amd64|x86_64) echo x86_64 ;; *) echo "$1" ;; esac; }

host_os=$(uname -s)
if [ "$universal" = 1 ]; then
  [ "$host_os" = Darwin ] || { echo "--macos-universal must be run on a Mac." >&2; exit 1; }
  os=macos; arch=universal; libc=""
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$t" >/dev/null 2>&1 || true
    cargo build --release --locked -p nyforge -p nyforge-gui --target "$t"
  done
  built=target/universal/nyforge
  built_gui=target/universal/nyforge-gui
  mkdir -p target/universal
  lipo -create -output "$built" target/aarch64-apple-darwin/release/nyforge target/x86_64-apple-darwin/release/nyforge
  lipo -create -output "$built_gui" target/aarch64-apple-darwin/release/nyforge-gui target/x86_64-apple-darwin/release/nyforge-gui
elif [ -n "$target" ]; then
  triple=${target%%.[0-9]*}           # drop a trailing ".2.17" glibc version
  case "$triple" in
    *-linux-*) os=linux ;;
    *-apple-darwin) os=macos ;;
    *) echo "package-unix.sh: cannot package for $triple" >&2; exit 2 ;;
  esac
  arch=$(norm_arch "${triple%%-*}")
  case "$triple" in *-musl) libc=-musl ;; *) libc="" ;; esac
  rustup target add "$triple" >/dev/null 2>&1 || true
  if [ "$os" = linux ] && [ "$(uname -s)" = Linux ]; then
    command -v cargo-zigbuild >/dev/null 2>&1 || { echo "Cross builds need cargo-zigbuild: pip install ziglang cargo-zigbuild" >&2; exit 1; }
    cargo zigbuild --release --locked -p nyforge --target "$target"
  else
    cargo build --release --locked -p nyforge --target "$triple"
  fi
  built=target/$triple/release/nyforge
  built_gui=""
else
  case "$host_os" in Linux) os=linux ;; Darwin) os=macos ;; *) echo "Unsupported host $host_os" >&2; exit 1 ;; esac
  arch=$(norm_arch "$(uname -m)"); libc=""
  cargo build --release --locked -p nyforge -p nyforge-gui
  built=target/release/nyforge
  built_gui=target/release/nyforge-gui
fi

prefix=nyforge
[ -z "$built_gui" ] && prefix=nyforge-cli
name=$prefix-$ver-$os-$arch$libc
rm -rf "dist/$name"
mkdir -p "dist/$name/bin"
cp "$built" "dist/$name/bin/nyforge"
cp install.sh LICENSE README.md THIRD-PARTY.md "dist/$name/"
if [ -n "$built_gui" ]; then
  cp "$built_gui" "dist/$name/bin/nyforge-gui"
  mkdir -p "dist/$name/packaging" "dist/$name/assets/logo"
  cp packaging/nyforge.desktop.in packaging/org.nyforge.engine.policy.in "dist/$name/packaging/"
  cp assets/logo/nyforge.svg assets/logo/nyforge-256.png "dist/$name/assets/logo/"
  if [ "$os" = macos ]; then
    # A normal double-clickable app. The engine sits next to the GUI inside it, which is where the app looks.
    app="dist/$name/NyForge.app/Contents"
    mkdir -p "$app/MacOS" "$app/Resources"
    cp "$built_gui" "$app/MacOS/nyforge-gui"
    cp "$built" "$app/MacOS/nyforge"
    cp assets/logo/nyforge.icns "$app/Resources/nyforge.icns"
    sed "s#@VERSION@#$ver#g" packaging/Info.plist.in > "$app/Info.plist"
  fi
fi
tar -C dist -czf "dist/$name.tar.gz" "$name"
( cd dist && { sha256sum "$name.tar.gz" 2>/dev/null || shasum -a 256 "$name.tar.gz"; } > "$name.tar.gz.sha256" )
echo "dist/$name.tar.gz"
