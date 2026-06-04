# hew examples

Real `{patch + review comments}` pulled from public GitHub PRs, for previewing
the review UI against realistic input.

| Example | Source PR | Language | Size | Threads |
|---|---|---|---|---|
| `rust-long-en` | [rust-lang/rust#137944](https://github.com/rust-lang/rust/pull/137944) | English | very long (~11k diff lines, 290 files) | 50 |
| `misskey-ja` | [misskey-dev/misskey#17463](https://github.com/misskey-dev/misskey/pull/17463) | Japanese | normal (~530 diff lines, 17 files) | 1 (root + 3 replies, incl. a `suggestion`) |

Each example is a pair:

- `<name>.patch` — plain cumulative unified diff (the PR's `.diff`)
- `<name>.comments.json` — inline review comments in hew's sidecar schema

## Run

```sh
hew examples/misskey-ja.patch   --comments examples/misskey-ja.comments.json
hew examples/rust-long-en.patch --comments examples/rust-long-en.comments.json
```

## Regenerate / add more

```sh
examples/fetch_pr.sh <owner/repo> <pr-number> <out-basename>
# e.g.
examples/fetch_pr.sh misskey-dev/misskey 17463 misskey-ja
```

Notes:

- The patch is fetched from the `.diff` URL (plain unified diff). `gh pr diff
  --patch` is **not** used — it emits git `format-patch` mailbox format
  (multi-commit, mail headers) which hew's unified-diff parser rejects.
- `to_sidecar.mjs` converts GitHub `pulls/.../comments` into the sidecar schema,
  grouping replies under their root thread (`in_reply_to_id`) and mapping
  `LEFT`→`old` / `RIGHT`→`new`.
- Comments on outdated/force-pushed positions that no longer anchor inside the
  current diff are dropped, so every thread in the example renders.
