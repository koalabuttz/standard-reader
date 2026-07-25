#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
web_dir="${repo_root}/crates/standard-reader-web"
website_dir="${1:-"${repo_root}/../website"}"
destination="${website_dir}/standard-reader/app"

if ! git -C "${website_dir}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "website checkout not found: ${website_dir}" >&2
  exit 1
fi

if [ -n "$(git -C "${website_dir}" status --porcelain -- standard-reader/app)" ]; then
  echo "refusing to replace website/standard-reader/app with uncommitted changes present" >&2
  exit 1
fi

(
  cd "${web_dir}"
  # Trunk 0.21 expects NO_COLOR to be the literal `true`/`false`; some agent shells export `1`.
  env -u NO_COLOR trunk build --release --locked
)

dist="${web_dir}/dist"
test -f "${dist}/index.html"
test -f "${dist}/sw.js"
test -n "$(find "${dist}" -maxdepth 1 -type f -name '*.js' -print -quit)"
test -n "$(find "${dist}" -maxdepth 1 -type f -name '*.wasm' -print -quit)"

# Trunk leaves indentation on blank lines where it consumes data-trunk links. Keep the vendored
# website artifact clean so its repository's whitespace checks stay useful.
sed -i 's/[[:space:]]*$//' "${dist}/index.html"

if ! grep -q '/standard-reader/app/' "${dist}/index.html"; then
  echo "generated index does not use /standard-reader/app/ asset URLs" >&2
  exit 1
fi

mkdir -p "${destination}"
rsync --archive --delete "${dist}/" "${destination}/"

echo "staged web release in ${destination}"
git -C "${website_dir}" status --short -- standard-reader/app
