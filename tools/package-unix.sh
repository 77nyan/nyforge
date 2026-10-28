#!/bin/sh
# Build a release archive for Linux from the current tree.
#
#   tools/package-unix.sh                                  native build for this machine: the desktop app
#                                                          (nyforge-gui) and the command line (nyforge)
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
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target=$2; shift ;;
    --target=*) target=${1#--target=} ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "package-unix.sh: unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

norm_arch() { case "$1" in arm64|aarch64) echo aarch64 ;; amd64|x86_64) echo x86_64 ;; *) echo "$1" ;; esac; }

host_os=$(uname -s)
if [ -n "$target" ]; then
  triple=${target%%.[0-9]*}           # drop a trailing ".2.17" glibc version
  case "$triple" in
    *-linux-*) os=linux ;;
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
  case "$host_os" in Linux) os=linux ;; *) echo "Unsupported host $host_os" >&2; exit 1 ;; esac
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
fi
tar -C dist -czf "dist/$name.tar.gz" "$name"
( cd dist && { sha256sum "$name.tar.gz" 2>/dev/null || shasum -a 256 "$name.tar.gz"; } > "$name.tar.gz.sha256" )
echo "dist/$name.tar.gz"
