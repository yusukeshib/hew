# hew

A fast, review-first terminal patch viewer, in Rust.

`hew` reads a **unified diff** and opens it in an interactive review UI where you
can attach GitHub-PR-style threaded comments. Source selection (working tree,
revs, two files) is delegated to git — you just pipe a diff in.

> *hew* = to cut/shape a block with an axe. Same "chunk/block" lineage as a diff
> `hunk`. Three letters, fast to type. Inspired by [hunk](https://github.com/modem-dev/hunk),
> rebuilt from zero as a native single binary.

## Install

```sh
cargo install --path .
# or
cargo build --release   # → target/release/hew
```

## Usage

`hew` consumes a unified patch and nothing else — from a file, or stdin:

```sh
hew change.patch                 # review a patch file
git diff HEAD | hew              # review the working tree (piped)
git show <rev> | hew             # review a commit
git diff <a> <b> | hew           # compare two refs
hew - < change.patch             # explicit stdin

hew change.patch --json          # print the parsed changeset as JSON, no TUI
```

Load review comments from a sidecar JSON file:

```sh
hew change.patch --comments review.json
```

### Options

| Flag | Meaning |
|---|---|
| `FILE` (positional) | Patch file to review. Omit or use `-` for stdin. |
| `--comments <FILE>` | Sidecar JSON of review comments to load. |
| `--json` | Print the parsed changeset as JSON and exit (no TUI). |

## Keys

| Key | Action |
|---|---|
| `j` / `k` (or ↓/↑) | Move selection |
| `g` / `G` | Jump to top / bottom |
| `V` | Start a range selection |
| `c` | Comment on the selected line/range |
| `r` | Reply to the thread |
| `R` | Toggle resolve |
| `d` | Delete thread |
| `n` / `N` | Jump to next / previous comment |
| `Esc` | Cancel input |
| `q` | Quit |

Comments live in memory only — nothing is written back to the patch or to disk.

## Comment sidecar format

```json
{
  "threads": [
    {
      "file": "src/main.rs",
      "side": "new",
      "range": { "start": 18, "end": 22 },
      "resolved": false,
      "comments": [
        { "author": "agent", "body": "This match arm is unreachable." },
        { "author": "you",   "body": "Good catch." }
      ]
    }
  ]
}
```

- `side`: `"new"` (added/context, RIGHT) or `"old"` (removed, LEFT)
- `range`: a single line uses `start == end`
- `comments[0]` is the thread root; the rest are replies
- `author`, `resolved`, `id`, `created_at` are optional (sensible defaults)

A bare `[ ...threads... ]` array is also accepted.

## Examples

Real `{patch + comments}` from public PRs live in [`examples/`](examples/):

```sh
hew examples/misskey-ja.patch   --comments examples/misskey-ja.comments.json
hew examples/rust-long-en.patch --comments examples/rust-long-en.comments.json
```

See [examples/README.md](examples/README.md) for how to fetch more.

## Design & roadmap

`hew` is intentionally a **read-only viewer**: no persistence, no GitHub/network
integration, no patch apply/edit/merge, no structural (AST) diff. Today it is a
single-pane unified viewer with threaded comments.

Planned: split/stack layouts and a file sidebar, syntax highlighting (syntect →
tree-sitter), `--watch` reload, and a loopback session server so an agent/CLI can
drive a running TUI.

Note: `hew` parses **plain unified diffs**, not git `format-patch` mailbox output
(`gh pr diff --patch`). Use a `.diff`/`git diff` stream instead.
