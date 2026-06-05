#!/usr/bin/env bash
# Replay a hew action log (stdin or $4) against a GitHub PR via `gh`.
#
# Usage: examples/apply_actions.sh <owner/repo> <pr-number> <commit-sha> [actions.json]
#        git diff | hew --comments base.json | examples/apply_actions.sh owner/repo 42 "$(git rev-parse HEAD)"
#
# This is a *convenience example*, never a dependency and never in the binary:
# hew only speaks the documented JSON; the GitHub round-trip lives out here.
#
# Scope / caveats:
#   - Handles `add_comment` (posts a new inline review comment). Multi-line
#     anchors use start_line..line, single-line just line.
#   - `reply` / `resolve` / `unresolve` / `delete` need to map hew's thread_id
#     (a UUID) back to a GitHub comment/thread id. That mapping is the
#     consumer's job (e.g. a sidecar your bridge wrote alongside the base);
#     this thin example only logs them. See README "Action log format".
set -euo pipefail

repo="$1"; pr="$2"; sha="$3"; src="${4:-/dev/stdin}"

jq -c '.[]' "$src" | while read -r action; do
  kind="$(jq -r '.action' <<<"$action")"
  case "$kind" in
    add_comment)
      path="$(jq -r '.file' <<<"$action")"
      body="$(jq -r '.body' <<<"$action")"
      line="$(jq -r '.line' <<<"$action")"
      start="$(jq -r '.start_line // empty' <<<"$action")"
      side="$(jq -r 'if .side == "old" then "LEFT" else "RIGHT" end' <<<"$action")"
      # `gh api -F` reads a value as a file when it starts with `@`, so a
      # non-numeric `line`/`start_line` from an untrusted actions.json could
      # exfiltrate a local file. Pass them as numbers only when they really are.
      case "$line" in ''|*[!0-9]*)
        echo "skip add_comment: non-numeric line '$line'" >&2; continue;;
      esac
      if [ -n "$start" ]; then
        case "$start" in *[!0-9]*)
          echo "skip add_comment: non-numeric start_line '$start'" >&2; continue;;
        esac
      fi
      args=(-f "body=$body" -f "commit_id=$sha" -f "path=$path" \
            -F "line=$line" -f "side=$side")
      if [ -n "$start" ]; then
        args+=(-F "start_line=$start" -f "start_side=$side")
      fi
      gh api "repos/$repo/pulls/$pr/comments" "${args[@]}" >/dev/null
      echo "posted comment on $path:${start:+$start-}$line"
      ;;
    reply|resolve|unresolve|delete)
      echo "skip $kind (needs thread_id->GitHub-id mapping; see header)" >&2
      ;;
    *)
      echo "unknown action: $kind" >&2
      ;;
  esac
done
