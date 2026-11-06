#!/bin/sh
set -eu

shim_dir="${CODEX_PLUS_PLUS_SHIM_DIR:-${HOME:-.}/.local/bin}"

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
    npm_platform=linux-x64
    archive_suffix=tar.gz
    ;;
  Darwin:arm64|Darwin:aarch64)
    target=aarch64-apple-darwin
    npm_platform=darwin-arm64
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
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/codex-plus-plus.XXXXXX")
trap 'rm -rf "$temp_dir"' 0
trap 'exit 1' HUP INT TERM
installer_name=install-codex-plus-plus.sh
registry=${CODEX_PLUS_PLUS_NPM_REGISTRY_URL:-https://registry.npmjs.org/@jjliebig%2fcodex-plus-plus}

npm_metadata() {
  LC_ALL=C fold -b -w 4096 | LC_ALL=C awk '
# Extract only the version document version, dist.tarball and dist.integrity.
# Keep lexical state across records so fold can bound compact registry JSON.
function unhex(s, n,j,c) {
    n=0
    for(j=1;j<=length(s);j++) {
        c=index("0123456789abcdef",tolower(substr(s,j,1)))-1
        if(c<0) return -1
        n=n*16+c
    }
    return n
}
{
    for(i=1;i<=length($0);i++) {
        c=substr($0,i,1)
        if(string) {
            if(unicode) {
                hex=hex c
                if(length(hex)==4) {
                    n=unhex(hex)
                    token=token (n>0 && n<128 ? sprintf("%c",n) : "?")
                    unicode=0
                }
            } else if(escape) {
                if(c=="u") { unicode=1; hex="" }
                else if(c=="/" || c=="\\" || c=="\"") token=token c
                else token=token "?"
                escape=0
            } else if(c=="\\") escape=1
            else if(c=="\"") {
                string=0
                if(value) {
                    if(depth==1 && key=="version") print "version\t" token
                    if(depth==2 && path[depth]=="dist" && (key=="tarball" || key=="integrity")) print key "\t" token
                    key=""; value=0
                } else pending=token
            } else token=token c
            continue
        }
        if(c=="\"") { string=1; token="" }
        else if(c==":") { key=pending; pending=""; value=1 }
        else if(c=="{" || c=="[") { depth++; path[depth]=key; key=""; value=0 }
        else if(c=="}" || c=="]") { depth--; key=""; value=0 }
        else if(c==",") { key=""; value=0 }
    }
}
END { if(string || depth!=0) exit 1 }
  '
}
field() { awk -F '\t' -v key="$1" '$1==key { print $2 }' "$temp_dir/npm-meta"; }
stable() { printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+-fork\.[0-9]+$'; }
newer() {
  awk -v a="$1" -v b="$2" 'BEGIN {
    gsub(/-fork\./,".",a); gsub(/-fork\./,".",b)
    split(a,x,"."); split(b,y,".")
    for(i=1;i<=4;i++) { if(x[i]+0>y[i]+0) exit 0; if(x[i]+0<y[i]+0) exit 1 }
    exit 1
  }'
}
npm_version=
if curl -fsSL --max-time 10 "$registry/$npm_platform" -o "$temp_dir/npm.json" &&
   npm_metadata < "$temp_dir/npm.json" > "$temp_dir/npm-meta"; then
  package_version=$(field version)
  npm_version=${package_version%-$npm_platform}
  stable "$npm_version" && [ "$package_version" = "$npm_version-$npm_platform" ] || npm_version=
fi
github_version=
if release_url=$(curl -fsSL --max-time 10 --output /dev/null --write-out '%{url_effective}' "$release_base/releases/latest"); then
  tag=${release_url%/}; tag=${tag##*/}
  github_version=${tag#codex-plus-plus-v}
  stable "$github_version" || github_version=
fi
if stable "${CODEX_PLUS_PLUS_CURRENT_VERSION:-}"; then
  [ -z "$npm_version" ] || ! newer "$CODEX_PLUS_PLUS_CURRENT_VERSION" "$npm_version" || npm_version=
  [ -z "$github_version" ] || ! newer "$CODEX_PLUS_PLUS_CURRENT_VERSION" "$github_version" || github_version=
fi

download() {
  name=$1
  curl --fail --silent --show-error --location \
    "$download_base/$name" --output "$temp_dir/$name" || {
    echo "Could not download required Codex++ release asset: $name" >&2
    return 1
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

github_package() {
  [ -n "$github_version" ] || return 1
  version=$github_version
  archive_name="codex-plus-plus-$version-$target.$archive_suffix"
  download_base="$release_base/releases/download/codex-plus-plus-v$version"
  download "$archive_name" && download "$archive_name.sha256" || return 1
  verify "$archive_name"
  mkdir -p "$temp_dir/github"
  tar -xzf "$temp_dir/$archive_name" -C "$temp_dir/github" || exit 1
  package_dir="$temp_dir/github"
  installer="$package_dir/codex-resources/install/$installer_name"
  if [ ! -f "$installer" ]; then
    download "$installer_name" && download "$installer_name.sha256" || return 1
    verify "$installer_name"
    installer="$temp_dir/$installer_name"
  fi
  [ -f "$package_dir/bin/codex" ] && [ -f "$package_dir/codex-package.json" ]
}

npm_package() {
  [ -n "$npm_version" ] || return 1
  tarball=$(field tarball)
  [ -n "$tarball" ] || return 1
  integrity=$(field integrity)
  printf '%s\n' "$integrity" | grep -Eq '^sha512-[A-Za-z0-9+/]{86}==$' || {
    echo "Invalid npm SHA-512 integrity." >&2; exit 1;
  }
  curl -fsSL "$tarball" -o "$temp_dir/npm.tgz" || return 1
  command -v openssl >/dev/null 2>&1 || { echo "openssl is required to verify npm payloads." >&2; exit 1; }
  actual=$(openssl dgst -sha512 -binary "$temp_dir/npm.tgz" | openssl base64 -A)
  [ "sha512-$actual" = "$integrity" ] || { echo "npm SHA-512 mismatch." >&2; exit 1; }
  mkdir "$temp_dir/npm"
  tar -xzf "$temp_dir/npm.tgz" -C "$temp_dir/npm" || exit 1
  package_dir="$temp_dir/npm/package/vendor/$target"
  [ -f "$package_dir/bin/codex" ] && [ -f "$package_dir/codex-package.json" ] || return 1
  installer="$package_dir/codex-resources/install/$installer_name"
  [ -f "$installer" ] || installer=${CODEX_PLUS_PLUS_INSTALLER_PATH:-}
  [ -f "$installer" ] || return 1
  version=$npm_version
}

if [ -n "$github_version" ] && { [ -z "$npm_version" ] || newer "$github_version" "$npm_version"; }; then
  github_package || npm_package || { echo "No usable published Codex++ package." >&2; exit 1; }
else
  npm_package || github_package || { echo "No usable published Codex++ package." >&2; exit 1; }
fi
sh "$installer" --target-exe "$package_dir/bin/codex" --shim-dir "$shim_dir" --install
