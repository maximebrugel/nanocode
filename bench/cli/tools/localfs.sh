#!/bin/sh
# Run a command where the files the CLIs map from a virtiofs mount come from copies on a local filesystem: localfs.sh cmd...
# virtiofs with DAX maps file pages from host memory, so smaps, statm and rusage count them as 0: a dynamic binary's
# libc.so.6 and ld.so pages, and node's own binary, drop out of RSS and PSS. The candidates are each lane binary in the
# environment (NC_BIN, NCLITE_BIN, FX_BIN, CLAUDE_BIN, CODEX_BIN, OPENCODE_BIN, the BENCH_NC_VARIANTS binaries and the
# executables beside each), node, sh, bash and ls, LOCALFS_FILES (colon-separated), their ldd closure and libnss_*. Each
# one on a virtiofs mount is copied to LOCALFS_DIR (default work/localfs, which must be on a local filesystem) and
# bind-mounted over its real path in a new mount namespace, entered without root through a user namespace mapped back to
# the caller's uid, else with sudo unshare and setpriv. With no candidate on virtiofs it runs the command as it is.
# Compose it outside tools/netguard.sh: localfs.sh netguard.sh node harness/suite.mjs ...
set -eu
[ $# -gt 0 ] || { echo "usage: localfs.sh cmd..." >&2; exit 2; }
dir=${LOCALFS_DIR:-$(cd "$(dirname "$0")/.." && pwd)/work/localfs}
mkdir -p "$dir"
list=$dir/mounts
seeds=$dir/seeds
: > "$seeds"
for b in "${NC_BIN:-}" "${NCLITE_BIN:-}" "${FX_BIN:-}" "${CLAUDE_BIN:-}" "${CODEX_BIN:-}" "${OPENCODE_BIN:-}" $(printf '%s' "${BENCH_NC_VARIANTS:-}" | tr , '\n' | sed -n 's/^[^=]*=//p'); do
  [ -n "$b" ] && [ -e "$b" ] || continue
  echo "$b" >> "$seeds"
  find "$(dirname "$b")" -maxdepth 1 -type f -perm -u+x >> "$seeds"
done
for c in node sh bash ls; do command -v "$c" >> "$seeds" || true; done
printf '%s' "${LOCALFS_FILES:-}" | tr : '\n' >> "$seeds"
libc=$(ldd "$(command -v sh)" | awk '$1 ~ /^libc\.so/ {print $3}')
[ -z "$libc" ] || ls "$(dirname "$libc")"/libnss_* >> "$seeds" 2>/dev/null || true
all=$dir/all
while read -r f; do
  [ -n "$f" ] && [ -e "$f" ] || continue
  echo "$f"
  ldd "$f" 2>/dev/null | awk '$2 == "=>" && $3 ~ /^\// {print $3} $1 ~ /^\// {print $1}'
done < "$seeds" | while read -r f; do realpath "$f"; done | sort -u > "$all"
: > "$list"
while read -r f; do
  [ "$(findmnt -n -o FSTYPE --target "$f")" = virtiofs ] || continue
  copy=$dir/files$f
  mkdir -p "$(dirname "$copy")"
  cmp -s "$f" "$copy" 2>/dev/null || cp -p "$f" "$copy"
  printf '%s %s\n' "$copy" "$f" >> "$list"
done < "$all"
[ -s "$list" ] || exec "$@"
echo "localfs: $(wc -l < "$list") files from virtiofs bind-mounted from $dir/files" >&2
U=$(id -u) G=$(id -g)
BIND='while read -r src dst; do mount --bind "$src" "$dst" || exit 1; done < "$0"'
if [ "${LOCALFS_MODE:-}" != sudo ] && unshare --user --map-root-user --mount true 2>/dev/null; then
  exec unshare --user --map-root-user --mount sh -c 'u=$1 g=$2 bind=$3; shift 3; eval "$bind" && exec unshare --user --map-user="$u" --map-group="$g" -- "$@"' "$list" "$U" "$G" "$BIND" "$@"
fi
exec sudo -n --preserve-env env PATH="$PATH" unshare --mount sh -c 'u=$1 g=$2 bind=$3; shift 3; eval "$bind" && exec setpriv --reuid="$u" --regid="$g" --init-groups -- "$@"' "$list" "$U" "$G" "$BIND" "$@"
