# hew

A fast, review-first terminal patch viewer, in Rust.

`hew` reads a **unified diff** and opens it in an interactive review UI,
displaying GitHub-PR-style threaded comments loaded from a sidecar JSON file.
Source selection (working tree, revs, two files) is delegated to git — you just
pipe a diff in.

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

Reload automatically when the patch or comments file changes on disk:

```sh
hew change.patch --comments review.json --watch
```

`--watch` reloads file inputs when they change on disk: edit `review.json` (or
regenerate the patch) in another window and the view refreshes. Watching only
applies to file inputs — a stdin patch can't be re-read.

### Options

| Flag | Meaning |
|---|---|
| `FILE` (positional) | Patch file to review. Omit or use `-` for stdin. |
| `--comments <FILE>` | Sidecar JSON of review comments to load. |
| `--name <NAME>` | Name this session in the registry (defaults to a short id). |
| `--json` | Print the parsed changeset as JSON and exit (no TUI). |
| `--watch` | Reload the patch/comments files when they change on disk. |

### Talking to a running session

While a `hew` TUI is open it advertises itself on a Unix socket under
`$XDG_RUNTIME_DIR/hew/` (or `/tmp/hew-$UID/`), so another process — you or an
AI agent — can read the live review:

```sh
hew comment list                 # the only running session
hew comment list --session pr-42  # by --name (or id) when several are open
```

`hew comment list` prints the session's current review store as JSON. (Writing
comments over the socket — `hew comment add/remove/resolve` — is coming next.)

## Keys

| Key | Action |
|---|---|
| `j` / `k` (or ↓/↑) | Move one line |
| `Ctrl-D` / `Ctrl-U` | Half page down / up |
| `Space` / `b` (or `Ctrl-F`/`Ctrl-B`, `PageDown`/`PageUp`) | Page down / up |
| `Ctrl-E` / `Ctrl-Y` | Scroll viewport one line (cursor stays in view) |
| `g` / `G` (or `Home`/`End`) | Jump to top / bottom |
| `[` / `]` | Jump to previous / next file |
| `n` / `N` | Jump to next / previous comment |
| `Enter` / `o` | Toggle the comment thread on the current line |
| `i` | Write a new comment on the current line |
| `r` | Reply to the thread on the current line |
| `R` | Resolve / unresolve the thread on the current line |
| `D` | Delete the thread on the current line |
| `←` / `→` | Focus the file list / the diff pane |
| `Ctrl-B` | Toggle the file list sidebar |
| `Tab` / `s` | Toggle unified ↔ split (side-by-side) layout |
| `y` | Copy the selected line(s) to the clipboard |
| `Esc` | Clear the line selection |
| `Ctrl-L` | Force a full repaint |
| `q` | Quit |

**Mouse**: click a file in the sidebar to open it, click a diff line to place the
cursor, **drag to select a range** of lines, and use the **wheel** to scroll the
pane under the pointer. Scrolling the file list just moves the list — it leaves
the selected file unchanged. **Drag the sidebar/diff divider** to resize the
sidebar. Both panes show a **scrollbar** when content overflows. `y` copies the
selection to the clipboard via OSC 52 (works in terminals that support it).

Multi-file diffs show a **file list sidebar**, grouped by directory (a dim
header per directory, files listed by basename with `+adds`/`-dels`, current
file highlighted), and the diff pane shows **only the selected file**. Keyboard
navigation acts on the **focused** pane: focus the sidebar with `←` and
`j`/`k`/`g`/`G` move between files (the diff follows); `→` (or `Enter`) returns to
the diff, where `j`/`k`/paging scroll within that one file. `[`/`]` switch files
from either pane. The focused pane's selection is brighter.

Unified stacks `-`/`+` lines; split shows old on the left and new on the right
(like `git delta --side-by-side`), pairing changed lines across a divider.
Toggling keeps the cursor on the same line.

Comments are loaded from a sidecar and displayed (gutter markers + inline
popup). You can resolve/delete threads in-app (`R`/`D`); the review store is
flushed back to the sidecar (or stdout) on exit.

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

`hew` stays intentionally small: no GitHub/network integration, no patch
apply/edit/merge, no structural (AST) diff. It loads a unified patch plus
sidecar review threads, lets you resolve/delete threads in-app, and flushes the
review back to JSON (or stdout) on exit. It offers unified and split layouts,
syntax highlighting (syntect + two-face's bat syntax set for broad language
coverage, Monokai Extended Bright theme, pure-Rust fancy-regex), sidecar comment
threads, and `--watch` reload.

Planned: a tree-sitter highlighting backend, theme selection, and a loopback
session server so an agent/CLI can drive a running TUI.

Note: `hew` parses **plain unified diffs**, not git `format-patch` mailbox output
(`gh pr diff --patch`). Use a `.diff`/`git diff` stream instead.
