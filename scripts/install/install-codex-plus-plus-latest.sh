#!/bin/sh
set -eu

shim_dir="${HOME:-.}/.local/bin"

usage() {
  echo "Usage: install-codex-plus-plus-latest.sh [--shim-dir DIR]"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --shim-dir)
      shim_dir=${2:?missing value for --shim-dir}
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

os_name=$(uname -s)
architecture=$(uname -m)
case "$os_name:$architecture" in
  Linux:x86_64|Linux:amd64)
    target=x86_64-unknown-linux-musl
    archive_suffix=tar.gz
    ;;
  Darwin:arm64|Darwin:aarch64)
    target=aarch64-apple-darwin
    archive_suffix=tar.gz
    ;;
  *)
    echo "Unsupported Codex++ install target: $os_name $architecture" >&2
    exit 1
    ;;
esac

command -v curl >/dev/null 2>&1 || {
  echo "curl is required to download Codex++." >&2
  exit 1
}
command -v tar >/dev/null 2>&1 || {
  echo "tar is required to extract Codex++." >&2
  exit 1
}
if command -v sha256sum >/dev/null 2>&1; then
  hash_command=sha256sum
elif command -v shasum >/dev/null 2>&1; then
  hash_command=shasum
else
  echo "sha256sum or shasum is required to verify Codex++." >&2
  exit 1
fi

release_base=${CODEX_PLUS_PLUS_RELEASE_BASE_URL:-https://github.com/JJLiebig/codex}
release_base=${release_base%/}
release_url=$(curl --fail --silent --show-error --location --output /dev/null \
  --write-out '%{url_effective}' "$release_base/releases/latest") || {
  echo "Could not resolve the latest stable Codex++ release." >&2
  exit 1
}
tag=${release_url%/}
tag=${tag##*/}
printf '%s\n' "$tag" | grep -Eq \
  '^codex-plus-plus-v[0-9]+\.[0-9]+\.[0-9]+-fork\.[0-9]+$' || {
  echo "Latest release is not a stable Codex++ release: $tag" >&2
  exit 1
}

version=${tag#codex-plus-plus-v}
archive_name="codex-plus-plus-$version-$target.$archive_suffix"
installer_name=install-codex-plus-plus.sh
download_base="$release_base/releases/download/$tag"
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/codex-plus-plus.XXXXXX")
trap 'rm -rf "$temp_dir"' 0
trap 'exit 1' HUP INT TERM

download() {
  name=$1
  curl --fail --silent --show-error --location \
    "$download_base/$name" --output "$temp_dir/$name" || {
    echo "Could not download required Codex++ release asset: $name" >&2
    exit 1
  }
}

hash_file() {
  if [ "$hash_command" = sha256sum ]; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

verify() {
  name=$1
  expected=$(awk -v name="$name" '
    NR == 1 && NF == 2 && length($1) == 64 &&
      $1 !~ /[^0-9A-Fa-f]/ && $2 == name { digest = tolower($1); next }
    { invalid = 1 }
    END {
      if (NR != 1 || invalid || digest == "") exit 1
      print digest
    }
  ' "$temp_dir/$name.sha256") || {
    echo "Malformed SHA-256 sidecar for Codex++ release asset: $name" >&2
    exit 1
  }
  actual=$(hash_file "$temp_dir/$name")
  [ "$actual" = "$expected" ] || {
    echo "SHA-256 mismatch for Codex++ release asset: $name" >&2
    exit 1
  }
}

download "$installer_name"
download "$installer_name.sha256"
verify "$installer_name"
download "$archive_name"
download "$archive_name.sha256"
verify "$archive_name"

package_dir="$temp_dir/package"
mkdir "$package_dir"
tar -xzf "$temp_dir/$archive_name" -C "$package_dir"
target_exe="$package_dir/bin/codex"
[ -f "$target_exe" ] || {
  echo "Verified Codex++ archive does not contain bin/codex." >&2
  exit 1
}

sh "$temp_dir/$installer_name" \
  --target-exe "$target_exe" \
  --shim-dir "$shim_dir" \
  --install
