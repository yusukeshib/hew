#!/usr/bin/env bash
# Fetch a GitHub PR's unified patch + inline review comments and emit a
# hew example pair: <out>.patch and <out>.comments.json (sidecar schema).
#
# Usage: examples/fetch_pr.sh <owner/repo> <pr-number> <out-basename>
set -euo pipefail

repo="$1"; pr="$2"; out="$3"
dir="$(cd "$(dirname "$0")" && pwd)"

# 1. The patch: plain cumulative unified diff via the .diff URL.
#    (Not `gh pr diff --patch` — that emits git format-patch mailbox format,
#    multi-commit with mail headers, which hew's unified-diff parser rejects.
#    The .diff URL also has no 20k-line API cap.)
curl -fsSL "https://github.com/$repo/pull/$pr.diff" -o "$dir/$out.patch"

# 2. Inline review comments -> sidecar threads JSON.
#    GitHub side LEFT->old, RIGHT->new. Group replies under their root via
#    in_reply_to_id so we get hew threads instead of flat comments.
gh api --paginate "repos/$repo/pulls/$pr/comments" \
  -H "Accept: application/vnd.github+json" > "$dir/$out.raw.json"

node "$dir/to_sidecar.mjs" "$dir/$out.raw.json" > "$dir/$out.comments.json"
rm -f "$dir/$out.raw.json"

echo "wrote $out.patch ($(wc -l < "$dir/$out.patch") lines) + $out.comments.json ($(node -e "console.log(require('$dir/$out.comments.json').threads.length)") threads)"
