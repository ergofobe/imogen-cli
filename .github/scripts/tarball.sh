#!/usr/bin/env bash
# Assembles one release tarball: the binary, the licence, the readme, the completions and
# the man page, under a single top-level directory so unpacking it never scatters files
# across whatever the person was standing in.
set -euo pipefail

TARGET="${1:?usage: tarball.sh <target-triple> <name>}"
NAME="${2:?usage: tarball.sh <target-triple> <name>}"

STAGE="$(mktemp -d)/${NAME}"
mkdir -p "$STAGE"

cp "target/${TARGET}/release/imogen" "$STAGE/"
cp README.md LICENSE "$STAGE/"
[ -d dist/completions ] && cp -R dist/completions "$STAGE/"
[ -d dist/man ] && cp -R dist/man "$STAGE/"

cat > "$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env sh
# Copies the binary somewhere on your PATH, and the completions and man page beside it if
# the usual directories exist. Nothing here needs root unless PREFIX says so.
set -eu
PREFIX="${PREFIX:-$HOME/.local}"
here="$(cd "$(dirname "$0")" && pwd)"

mkdir -p "$PREFIX/bin"
install -m 755 "$here/imogen" "$PREFIX/bin/imogen"
echo "installed $PREFIX/bin/imogen"

if [ -f "$here/man/imogen.1" ]; then
  mkdir -p "$PREFIX/share/man/man1"
  install -m 644 "$here/man/imogen.1" "$PREFIX/share/man/man1/imogen.1"
fi
for pair in \
  "completions/imogen.bash:share/bash-completion/completions/imogen" \
  "completions/_imogen:share/zsh/site-functions/_imogen" \
  "completions/imogen.fish:share/fish/vendor_completions.d/imogen.fish"
do
  src="$here/${pair%%:*}"
  dest="$PREFIX/${pair#*:}"
  [ -f "$src" ] || continue
  mkdir -p "$(dirname "$dest")"
  install -m 644 "$src" "$dest"
done

case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) echo "note: $PREFIX/bin is not on your PATH" ;;
esac
INSTALL
chmod +x "$STAGE/install.sh"

mkdir -p dist
tar -C "$(dirname "$STAGE")" -czf "dist/${NAME}.tar.gz" "${NAME}"
echo "dist/${NAME}.tar.gz"
tar -tzf "dist/${NAME}.tar.gz"
