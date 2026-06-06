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
```

Load existing review comments from a sidecar JSON file. This file is an
**immutable input** — hew reads it as the review's starting point and never
writes back to it:

```sh
hew change.patch --comments review.json
```

On exit, hew prints a **compacted action log** (what the session changed) to
stdout — it never modifies its inputs:

```sh
git diff | hew --comments base.json > actions.json
```

The log is the minimal set of actions (`add_comment`, `reply`, `resolve`,
`unresolve`) that turn `base.json` into the reviewed state; a thread created then
deleted, or a resolve toggled back, cancels out. An untouched session prints
`[]`. A consumer (e.g. a GitHub bridge) replays the log against the same base.

Comments loaded from `base.json` are immutable: `D` deletes a single comment,
and only ones you add in the session (e.g. a reply you wrote) — never a whole
input thread. So the log never contains a delete action (an in-session add and
delete simply cancel to nothing); deleting a thread's last comment just drops it.

> **`id` is an opaque string, kept verbatim.** Threads and comments carry an
> `id` that is any string — a UUID, or a foreign id such as a GitHub comment id.
> hew preserves it exactly as written, so the action log references the same ids
> as `base.json` and is replayable (a producer like a GitHub bridge can use real
> GitHub ids directly). A handwritten sidecar that omits `id` gets a fresh id at
> load, so its `resolve`/`reply` actions won't match the on-disk base — fine for
> ad-hoc viewing, not for replay.

### Options

| Flag | Meaning |
|---|---|
| `FILE` (positional) | Patch file to review. Omit or use `-` for stdin. |
| `--comments <FILE>` | Sidecar JSON of existing review comments to load (immutable). |

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
| `v` | Visual line-select: extend with `j`/`k`, then `i` anchors a comment to the range |
| `i` | Write a new comment on the current line (or the visual/drag selection) |
| `r` | Reply to the thread on the current line |
| `R` | Resolve / unresolve the thread on the current line |
| `D` | Delete the focused comment (only ones you added this session; input comments are immutable) |
| `←` / `→` | Focus the file list / the diff pane |
| `Ctrl-B` | Toggle the file list sidebar |
| `Tab` / `s` | Toggle unified ↔ split (side-by-side) layout |
| `y` | Copy the selected line(s) to the clipboard |
| `Esc` | Leave visual mode / clear the line selection |
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

The UI uses the GitHub Dark High Contrast theme. The whole palette — chrome
(sidebar, borders, headers, cursor line, status) and background — is *derived
from the syntax theme*, so the entire look comes from one source. Colors are
24-bit truecolor; hew checks `COLORTERM` and, on a non-truecolor terminal
(including tmux without truecolor passthrough), automatically downsamples to the
nearest xterm-256 colors so the look degrades gracefully. For the best fidelity,
enable truecolor — e.g. in tmux: `set -ga terminal-features "*:RGB"` (and make
sure `COLORTERM=truecolor` reaches the session).

Comments are loaded from a sidecar (immutable) and displayed (gutter markers +
inline popup). You can compose/reply/resolve threads and delete your own
comments in-app; on exit hew
prints the compacted action log to stdout (the inputs are never written).

The comment composer is a multi-line editor with readline/emacs-style keys:
`Ctrl-A`/`Ctrl-E` (line start/end), `Ctrl-B`/`Ctrl-F` and `←`/`→` (char),
`Alt-B`/`Alt-F` (word), `↑`/`↓` (line), `Ctrl-K`/`Ctrl-U` (kill to end/start),
`Ctrl-W` (delete word), `Ctrl-D` (delete forward), plus undo/redo. `Enter`
inserts a newline, `Ctrl-S` (or `Ctrl-Enter`) submits, `Esc` (or `Ctrl-C`)
cancels.

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

## Action log format (output)

On exit hew prints a JSON **array of actions** to stdout — the minimal delta that
turns the `--comments` base into the reviewed state. This is the program's
result: an agent reads it to drive `gh` (post comments, resolve threads), feeds
it into the next step, or audits it. No action types beyond these five are ever
emitted:

```json
[
  { "action": "add_comment", "thread_id": "<id>", "file": "src/main.rs",
    "side": "new", "start_line": 18, "line": 22, "body": "This arm is unreachable.", "author": "you" },
  { "action": "reply",     "thread_id": "<id>", "body": "Good catch.", "author": "you" },
  { "action": "resolve",   "thread_id": "<id>" },
  { "action": "unresolve", "thread_id": "<id>" }
]
```

- `add_comment` is a new thread's root, anchored to `(file, side, line)`. `line`
  is the thread's last line (GitHub's anchor); `start_line` is present only for a
  multi-line range and omitted for a single line (matching GitHub's
  `start_line`/`line` review-comment shape). Its `thread_id` is reused by any
  `reply` to the same thread within the log.
- `reply` / `resolve` / `unresolve` reference an existing thread by `thread_id`,
  an opaque string echoing the base `id` verbatim (a base thread, or one
  `add_comment`-ed earlier in the same log).
- `author` is omitted when unset.
- An untouched session prints `[]`.
- There is no `delete` action: input threads can't be deleted, and an in-session
  add-then-delete cancels out. Resolve-then-unresolve cancels too.

There is **no GitHub-specific code in hew** — the binary only speaks this JSON.
The "bridge" to GitHub is whoever consumes the log: an agent that knows this
schema can read `gh api` PR threads into a base sidecar and replay the action
log back through `gh`, with no wrapper binary required. Thin, convenience-only
examples of both directions ship in `examples/`: `fetch_pr.sh` prepares the base
sidecar from a PR, and `apply_actions.sh` replays the log's `add_comment`s via
`gh` (reply/resolve need the consumer's `thread_id`→GitHub-id mapping).

## Examples

Real `{patch + comments}` from public PRs live in [`examples/`](examples/):

```sh
hew examples/misskey-ja.patch   --comments examples/misskey-ja.comments.json
hew examples/rust-long-en.patch --comments examples/rust-long-en.comments.json
```

See [examples/README.md](examples/README.md) for how to fetch more.

## Design & roadmap

`hew` stays intentionally small and is a **pure filter**: no GitHub/network
integration, no patch apply/edit/merge, no structural (AST) diff, and no "save"
— its inputs (the patch and the `--comments` base) are immutable, and it emits a
compacted action log to stdout on exit. It offers unified and split layouts,
in-app authoring (compose/reply/resolve/delete), syntax highlighting (syntect +
two-face's bat syntax set for broad language coverage, GitHub Dark High Contrast theme (chrome
palette derived from it),
pure-Rust fancy-regex), and sidecar comment threads.

Because hew is a pure filter with a documented JSON contract (see *Comment
sidecar format* and *Action log format* above), the GitHub round-trip needs no
code in the binary: an agent that understands the schema is the bridge — it
prepares the base sidecar from `gh api` and replays the action log through `gh`.

### Design invariants

These are the rules that keep hew small — they should not be broken:

- **hew never talks to GitHub.** It eats a patch + a comment JSON, nothing else.
  The GitHub round-trip lives entirely outside the binary; any `gh` wrappers
  ship only as `examples/`, never as a dependency.
- **hew is a pure filter — no "save".** All inputs are immutable: the patch
  (stdin) and the `--comments` base JSON are read, never written. There is no
  save/flush/autosave/document concept.
- **The TUI is the sole writer of its in-memory store.** All edits
  (compose/reply/resolve/delete) mutate one `CommentStore`.
- **Output is a compacted action log**, not the comment store: on exit hew emits
  `diff(base, final)` as a minimal action array to stdout (a thread created then
  deleted, or a resolve toggled back, cancels out). Replay requires the base to
  carry stable thread ids.
- **Channels stay separated:** stdin = patch, stderr/tty = render, stdout =
  action-log result.
- **No daemon, no DB, no background services**, and a deliberately minimal CLI
  (just `FILE` and `--comments`).

### Planned

A tree-sitter highlighting backend and theme selection. Optional `gh` wrapper
examples may ship under `examples/`, but they are conveniences, not a dependency.
Live in-session AI co-review over a socket is deferred and out of scope for v1;
the turn-based flow (an agent prepares a base review, opens hew for the human,
then drives `gh` from the emitted action log) covers the v1 workflows without it.

Note: `hew` parses **plain unified diffs**, not git `format-patch` mailbox output
(`gh pr diff --patch`). Use a `.diff`/`git diff` stream instead.
