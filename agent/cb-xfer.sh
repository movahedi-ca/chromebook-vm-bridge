#!/bin/bash
# cb-xfer.sh - rsync file transfer between this VM and chromebook-vm.
#
# Phase 2A item 7: replaces `cat local | cb-ssh.sh 'cat > remote'` with
# rsync -a --checksum --partial --compress over cb-ssh.sh, plus a sha256
# integrity check after every transfer. SSH -C compression is enabled for
# text-heavy transfers via CB_SSH_FLAGS (rsync's own --compress already skips
# files it knows are compressed; -C additionally compresses the whole stream
# including rsync protocol overhead).
#
# Usage:
#   cb-xfer.sh push LOCAL REMOTE   # this VM -> chromebook-vm
#   cb-xfer.sh pull REMOTE LOCAL   # chromebook-vm -> this VM
#
# Directory paths follow rsync trailing-slash semantics (a trailing slash on
# the source means "contents of"). Every transfer ends with xfer-ok plus the
# verified sha256, or fails loudly.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
CB_SSH="$HERE/cb-ssh.sh"
# Uses CB_HOST from the environment (same as cb-ssh.sh).
: "${CB_HOST:?cb-xfer: CB_HOST is not set}"
HOST="$CB_HOST"

usage() { echo "usage: cb-xfer.sh push LOCAL REMOTE | pull REMOTE LOCAL" >&2; exit 2; }
fail() { echo "cb-xfer: $1" >&2; exit "${2:-1}"; }

[ $# -eq 3 ] || usage
mode="$1"; src="$2"; dst="$3"
case "$mode" in push|pull) ;; *) usage;; esac
[ -x "$CB_SSH" ] || fail "cb-ssh.sh missing or not executable: $CB_SSH"
command -v rsync >/dev/null 2>&1 || fail "rsync not installed on this VM"
"$CB_SSH" 'command -v rsync' >/dev/null 2>&1 || fail "rsync not installed on chromebook-vm"

# _q: shell-quote one path for the remote shell.
_q() { printf '%q' "$1"; }

sha_local_file() { sha256sum -- "$1" | cut -d' ' -f1; }
sha_remote_file() { "$CB_SSH" "sha256sum -- $(_q "$1")" | cut -d' ' -f1; }
manifest_local() {
  ( cd -- "$1" && find . -type f -print0 | sort -z | xargs -0 sha256sum -- )
}
manifest_remote() {
  "$CB_SSH" "cd -- $(_q "$1") && find . -type f -print0 | sort -z | xargs -0 sha256sum --"
}

verify() { # kind local_path remote_path  (kind: file|dir)
  local kind="$1" lp="$2" rp="$3" a b
  if [ "$kind" = "dir" ]; then
    a="$(manifest_local "$lp")"; b="$(manifest_remote "$rp")"
  else
    a="$(sha_local_file "$lp")"; b="$(sha_remote_file "$rp")"
  fi
  [ -n "$a" ] && [ -n "$b" ] || fail "integrity check: empty checksum (a='${a:-?}' b='${b:-?}')"
  if [ "$a" = "$b" ]; then
    echo "xfer-ok $mode $src -> $dst integrity=sha256:$(printf '%s' "$a" | cut -c1-16)..."
  else
    fail "INTEGRITY MISMATCH after $mode $src -> $dst (local ${a:0:16}... vs remote ${b:0:16}...); remote copy left in place for inspection"
  fi
}

export CB_SSH_FLAGS="-C"
if [ "$mode" = "push" ]; then
  export CB_AUDIT_CMD="xfer:push"
  [ -e "$src" ] || fail "local source missing: $src"
  if [ -d "$src" ]; then
    kind=dir; lroot="$src"; rsrc="$src/"; rroot="$dst"  # dir: sync contents into dst
  else
    kind=file; lroot="$src"; rsrc="$src"; rroot="$dst"
  fi
  # Transfer-level retry (see pull branch): safe and idempotent via
  # --partial --checksum.
  attempt=1
  while :; do
    if rsync -a --checksum --partial --compress -e "$CB_SSH" -- "$rsrc" "$HOST:$dst"; then
      break
    fi
    rc=$?
    if [ "$attempt" -ge 3 ]; then fail "rsync push failed after $attempt attempts (last exit $rc)"; fi
    sleep_ms=$(( 1000 * attempt + RANDOM % 1000 ))
    sleep "$(awk -v ms="$sleep_ms" 'BEGIN{printf "%.3f", ms/1000}')"
    attempt=$(( attempt + 1 ))
  done
  verify "$kind" "$lroot" "$rroot"
else
  export CB_AUDIT_CMD="xfer:pull"
  # Transfer-level retry (phase 2A item 3 applied at the transfer layer): if
  # the ssh master dies mid-transfer the wrapper cannot resume rsync, so
  # retry the whole rsync; --partial --checksum make this safe and idempotent.
  rkind="$("$CB_SSH" "if [ -d $(_q "$src") ]; then echo dir; elif [ -e $(_q "$src") ]; then echo file; else echo missing; fi")"
  [ "$rkind" = "missing" ] && fail "remote source missing: $src"
  kind="$rkind"
  attempt=1
  while :; do
    if rsync -a --checksum --partial --compress -e "$CB_SSH" -- "$HOST:$src" "$dst"; then
      break
    fi
    rc=$?
    if [ "$attempt" -ge 3 ]; then fail "rsync pull failed after $attempt attempts (last exit $rc)"; fi
    sleep_ms=$(( 1000 * attempt + RANDOM % 1000 ))
    sleep "$(awk -v ms="$sleep_ms" 'BEGIN{printf "%.3f", ms/1000}')"
    attempt=$(( attempt + 1 ))
  done
  # Resolve where rsync actually landed the payload (empirical, not assumed):
  # a dir pulled without trailing slash nests as dst/<basename>; a file pulled
  # into an existing dir lands as dst/<basename>, else dst itself.
  if [ "$kind" = "dir" ]; then
    if [ -d "$dst/$(basename "$src")" ]; then lroot="$dst/$(basename "$src")"
    elif [ -d "$dst" ]; then lroot="$dst"
    else fail "pull: expected directory not found under $dst"; fi
  else
    if [ -d "$dst" ]; then lroot="$dst/$(basename "$src")"
    elif [ -f "$dst" ]; then lroot="$dst"
    else fail "pull: expected file not found at $dst"; fi
  fi
  verify "$kind" "$lroot" "$src"
fi
