#!/bin/sh
set -eu

target_exe=
shim_dir="${HOME:-.}/.local/bin"
install=0
remove=0

usage() {
  cat <<'EOF'
Usage: install-codex-plus-plus.sh --target-exe PATH [--shim-dir DIR] [--install]
       install-codex-plus-plus.sh [--shim-dir DIR] --remove
EOF
}

abs_path() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s/%s\n' "$(pwd -P)" "$1" ;;
  esac
}

shell_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

owner_is_live() {
  owner_path=$1
  lock_pid=$(sed -n '1p' "$owner_path" 2>/dev/null || true)
  lock_start=$(sed -n '2p' "$owner_path" 2>/dev/null || true)
  live_start=
  if [ -n "$lock_pid" ]; then
    live_start=$(LC_ALL=C ps -o lstart= -p "$lock_pid" 2>/dev/null || true)
  fi
  [ -n "$lock_start" ] && [ "$live_start" = "$lock_start" ]
}

acquire_install_lock() {
  lock_root="$shim_dir/.codex-plus-plus-install-locks"
  mkdir -p "$lock_root"
  process_start=$(LC_ALL=C ps -o lstart= -p "$$")
  if [ -z "$process_start" ]; then
    echo "Could not read the installer process identity." >&2
    exit 1
  fi
  process_identity=$(printf '%s' "$process_start" | tr -cd '[:alnum:]')
  choosing_path="$lock_root/choosing.$$.$process_identity"
  owner_temp="$lock_root/.owner.$$.$process_identity.tmp"
  {
    printf '%s\n' "$$"
    printf '%s\n' "$process_start"
  } > "$owner_temp"
  mv "$owner_temp" "$choosing_path"

  ticket_number=1
  for other_ticket in "$lock_root"/ticket.*; do
    [ -f "$other_ticket" ] || continue
    ticket_base=${other_ticket##*/ticket.}
    other_number=${ticket_base%%.*}
    case "$other_number" in
      *[!0-9]*|'') continue ;;
    esac
    if [ "$other_number" -ge "$ticket_number" ]; then
      ticket_number=$((other_number + 1))
    fi
  done
  ticket_path="$lock_root/ticket.$ticket_number.$$.$process_identity"
  ticket_temp="$lock_root/.ticket.$$.$process_identity.tmp"
  cp "$choosing_path" "$ticket_temp"
  mv "$ticket_temp" "$ticket_path"
  rm -f "$choosing_path"

  trap 'unlock' 0
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  waited=0
  while :; do
    blocked=0
    for other_owner in "$lock_root"/choosing.*; do
      [ -f "$other_owner" ] || continue
      if owner_is_live "$other_owner"; then
        blocked=1
      else
        rm -f "$other_owner"
      fi
    done
    for other_ticket in "$lock_root"/ticket.*; do
      [ -f "$other_ticket" ] || continue
      [ "$other_ticket" = "$ticket_path" ] && continue
      if ! owner_is_live "$other_ticket"; then
        rm -f "$other_ticket"
        continue
      fi
      ticket_base=${other_ticket##*/ticket.}
      other_number=${ticket_base%%.*}
      ticket_rest=${ticket_base#*.}
      other_pid=${ticket_rest%%.*}
      if [ "$other_number" -lt "$ticket_number" ] || {
        [ "$other_number" -eq "$ticket_number" ] && [ "$other_pid" -lt "$$" ]
      }; then
        blocked=1
      fi
    done
    [ "$blocked" -eq 0 ] && break
    waited=$((waited + 1))
    if [ "$waited" -ge 600 ]; then
      echo "Timed out waiting for the Codex++ install lock." >&2
      exit 1
    fi
    sleep 0.1
  done
}

unlock() {
  rm -f "$ticket_path" "$choosing_path" "$owner_temp" "$ticket_temp"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --target-exe)
      target_exe=${2:?missing value for --target-exe}
      shift 2
      ;;
    --shim-dir)
      shim_dir=${2:?missing value for --shim-dir}
      shift 2
      ;;
    --install)
      install=1
      shift
      ;;
    --remove)
      remove=1
      shift
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

shim_dir=$(abs_path "$shim_dir")
shim_path="$shim_dir/codex"
current_path="$shim_dir/.codex-plus-plus-current"

if [ "$remove" -eq 1 ]; then
  mkdir -p "$shim_dir"
  shim_dir=$(CDPATH= cd "$shim_dir" && pwd -P)
  shim_path="$shim_dir/codex"
  current_path="$shim_dir/.codex-plus-plus-current"
  acquire_install_lock
  if [ -f "$shim_path" ]; then
    rm -f "$shim_path" "$current_path"
    echo "==> Removed shim at $shim_path"
  else
    echo "==> No shim found at $shim_path"
  fi
  exit 0
fi

if [ -z "$target_exe" ]; then
  echo "--target-exe is required unless --remove is set" >&2
  usage >&2
  exit 2
fi

target_path=$(abs_path "$target_exe")
active_codex=$(command -v codex 2>/dev/null || printf 'not found on PATH')

echo "==> Codex++ shim"
echo "Active codex path: $active_codex"
echo "Shim path: $shim_path"
echo "Target fork executable: $target_path"
if [ -f "$target_path" ]; then
  echo "Target reachable: true"
else
  echo "Target reachable: false"
fi

if [ "$install" -eq 0 ]; then
  echo "==> Dry run only; pass --install to write the shim."
  exit 0
fi

if [ ! -f "$target_path" ]; then
  echo "Target fork executable does not exist: $target_path" >&2
  exit 1
fi

target_bin_dir=$(CDPATH= cd "$(dirname "$target_path")" && pwd -P)
target_path="$target_bin_dir/$(basename "$target_path")"
if [ "$(basename "$target_bin_dir")" != bin ]; then
  echo "Target fork executable must be inside a package bin directory: $target_path" >&2
  exit 1
fi
package_dir=$(dirname "$target_bin_dir")
if [ ! -f "$package_dir/codex-package.json" ]; then
  echo "Target fork executable must belong to a Codex package: $target_path" >&2
  exit 1
fi

codex_home=${CODEX_HOME:-"${HOME:-.}/.codex"}
mkdir -p "$shim_dir" "$codex_home"
shim_dir=$(CDPATH= cd "$shim_dir" && pwd -P)
codex_home=$(CDPATH= cd "$codex_home" && pwd -P)
shim_path="$shim_dir/codex"
current_path="$shim_dir/.codex-plus-plus-current"
install_root="$codex_home/packages/codex-plus-plus"
releases_root="$install_root/releases"
leases_root="$shim_dir/.codex-plus-plus-leases"
mkdir -p "$releases_root"
releases_root=$(CDPATH= cd "$releases_root" && pwd -P)
set -- $(printf '%s' "$shim_dir" | cksum)
shim_id="$1-$2"
releases_dir="$releases_root/$shim_id"

case "$shim_dir/" in
  "$package_dir/"*)
    echo "Codex++ managed install paths must be outside the source package: $package_dir" >&2
    exit 1
    ;;
esac
case "$releases_root/" in
  "$package_dir/"*)
    echo "Codex++ managed install paths must be outside the source package: $package_dir" >&2
    exit 1
    ;;
esac
case "$shim_dir/" in
  "$releases_root/"*)
    echo "Codex++ shim and release directories must not overlap." >&2
    exit 1
    ;;
esac
case "$releases_root/" in
  "$shim_dir/"*)
    echo "Codex++ shim and release directories must not overlap." >&2
    exit 1
    ;;
esac

mkdir -p "$releases_dir" "$leases_root"
acquire_install_lock

previous_release=
if [ -f "$current_path" ]; then
  previous_generation=$(sed -n '1p' "$current_path")
  previous_release="$releases_dir/$previous_generation"
fi

release_name="$(date -u +%Y%m%dT%H%M%SZ)-$$"
release_dir="$releases_dir/$release_name"
staging_dir="$releases_dir/.staging.$release_name"
for abandoned_staging in "$releases_dir"/.staging.*; do
  [ -e "$abandoned_staging" ] || continue
  rm -rf "$abandoned_staging"
done
mkdir "$staging_dir"
cp -R "$package_dir/." "$staging_dir"
mv "$staging_dir" "$release_dir"
mkdir "$leases_root/$release_name"

shim_tmp="$shim_path.$$.tmp"
quoted_shim_dir=$(shell_quote "$shim_dir")
quoted_current_path=$(shell_quote "$current_path")
quoted_releases_dir=$(shell_quote "$releases_dir")
quoted_leases_root=$(shell_quote "$leases_root")
{
  echo '#!/bin/sh'
  echo 'set -u'
  echo "export CODEX_PLUS_PLUS_SHIM_DIR=$quoted_shim_dir"
  echo "current_path=$quoted_current_path"
  echo "releases_dir=$quoted_releases_dir"
  echo "leases_root=$quoted_leases_root"
  cat <<'EOF'
# Pass the pane credential to Codex only, after choosing the generation.
set +x
unset codex_plus_plus_resume_token
codex_plus_plus_resume_token=${OSC7577_TOKEN-}
unset OSC7577_TOKEN
while :; do
  generation=$(sed -n '1p' "$current_path" 2>/dev/null) || exit 1
  lease_dir="$leases_root/$generation"
  marker="$lease_dir/sh.$$"
  if [ -e "$lease_dir/.pruning" ] || [ -e "$leases_root/$generation.pruned" ]; then
    continue
  fi
  process_start=$(LC_ALL=C ps -o lstart= -p "$$") || exit 1
  printf '%s\n' "$process_start" > "$marker" 2>/dev/null || continue
  if [ -e "$lease_dir/.pruning" ] || [ -e "$leases_root/$generation.pruned" ]; then
    rm -f "$marker"
    continue
  fi
  break
done
if [ -n "$codex_plus_plus_resume_token" ]; then
  export OSC7577_TOKEN="$codex_plus_plus_resume_token"
fi
exec "$releases_dir/$generation/bin/codex" "$@"
EOF
} > "$shim_tmp"
chmod +x "$shim_tmp"
mv -f "$shim_tmp" "$shim_path"

current_tmp="$current_path.$$.tmp"
printf '%s\n' "$release_name" > "$current_tmp"
mv -f "$current_tmp" "$current_path"

for stale_release in "$releases_dir"/*; do
  [ -d "$stale_release" ] || continue
  [ "$stale_release" = "$release_dir" ] && continue
  [ -n "$previous_release" ] && [ "$stale_release" = "$previous_release" ] && continue
  stale_generation=$(basename "$stale_release")
  lease_dir="$leases_root/$stale_generation"
  pruning_gate="$lease_dir/.pruning"
  mkdir -p "$lease_dir"
  rm -f "$pruning_gate"
  : > "$pruning_gate"
  active=0
  for marker in "$lease_dir"/sh.*; do
    [ -f "$marker" ] || continue
    marker_pid=${marker##*/sh.}
    marker_start=$(sed -n '1p' "$marker" 2>/dev/null || true)
    live_start=$(LC_ALL=C ps -o lstart= -p "$marker_pid" 2>/dev/null || true)
    if [ -n "$marker_start" ] && [ "$live_start" = "$marker_start" ]; then
      active=1
    else
      rm -f "$marker"
    fi
  done
  if [ "$active" -eq 1 ]; then
    rm -f "$pruning_gate"
    echo "==> Kept active Codex++ release at $stale_release"
    continue
  fi
  : > "$leases_root/$stale_generation.pruned"
  rm -rf "$stale_release" "$lease_dir"
  echo "==> Removed stale Codex++ release at $stale_release"
done

echo "==> Installed shim at $shim_path"
echo "==> Active release: $release_dir"
if [ "$(command -v codex 2>/dev/null || true)" = "$shim_path" ]; then
  echo "==> Run: codex"
else
  echo "==> Run now: PATH=\"$shim_dir:\$PATH\" codex"
fi
