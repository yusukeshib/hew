# hew

A fast, review-first terminal patch viewer, in Rust.

`hew` reads a **unified diff** on stdin and opens it in an interactive review UI
with GitHub-PR-style threaded comments. It is a **pure filter**: the patch and an
optional comment sidecar are immutable inputs, and on exit hew prints a compacted
**action log** to stdout. Picking what to review (working tree, a rev, two refs)
is left to git — you just pipe a diff in.

## Install

The crate is `hewdiff` (the `hew` name was taken on crates.io); the installed
binary is `hew`.

```sh
cargo install hewdiff                              # from crates.io
nix profile install github:yusukeshib/hew          # or with Nix (flake)
nix run github:yusukeshib/hew -- change.patch      #    run without installing
```

From source: `git clone … && cd hew && cargo install --path .`.

## Usage

```sh
hew change.patch                 # review a patch file
git diff HEAD | hew              # review the working tree (piped)
git show <rev> | hew             # review a commit
git diff <a> <b> | hew           # compare two refs
hew - < change.patch             # explicit stdin
```

Load existing review comments from a sidecar JSON file (an **immutable** starting
point — hew never writes back to it):

```sh
hew change.patch --comments review.json
```

On exit hew prints a **compacted action log** to stdout — the minimal set of
actions (`add_comment`, `reply`, `resolve`, `unresolve`) that turns the
`--comments` base into the reviewed state. A consumer (e.g. a GitHub bridge)
replays it against the same base:

```sh
git diff | hew --comments base.json > actions.json
```

Compaction is automatic: a thread added then deleted, or a resolve toggled back,
cancels out. An untouched session prints `[]`. See
[Action log format](#action-log-format-output) for the schema.

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
| `w` | Toggle soft-wrap of long diff lines (on by default; off clips at the right edge) |
| `y` | Copy the selected line(s) to the clipboard (OSC 52) |
| `Esc` | Leave visual mode / clear the line selection |
| `Ctrl-L` | Force a full repaint |
| `q` | Quit |

**Mouse**: click a sidebar file to open it, click a diff line to place the
cursor, drag to select a range, and use the wheel to scroll the pane under the
pointer. Drag the sidebar/diff divider to resize it. Both panes show a scrollbar
when content overflows.

### Layout & navigation

Multi-file diffs show a **file list sidebar** grouped by directory (files by
basename with `+adds`/`-dels`), and the diff pane shows only the selected file.
Keyboard navigation acts on the **focused** pane (its selection is brighter):
focus the sidebar with `←` and `j`/`k`/`g`/`G` move between files; `→` (or
`Enter`) returns to the diff, where `j`/`k`/paging scroll within that file.
`[`/`]` switch files from either pane.

**Unified** stacks `-`/`+` lines; **split** shows old on the left and new on the
right (like `git delta --side-by-side`). Toggling keeps the cursor on the same
line.

The UI uses **Hew Dark**, our own vivid, high-contrast palette over a deep
navy-slate background. The whole palette — chrome and background — is *derived
from the syntax theme*, so the look comes from one source. Colors are 24-bit truecolor; on a non-truecolor terminal (including tmux
without RGB passthrough) hew downsamples to xterm-256 via `COLORTERM`. For best
fidelity enable truecolor — e.g. in tmux: `set -ga terminal-features "*:RGB"`.

The comment **composer** is a multi-line editor with readline/emacs keys:
`Ctrl-A`/`Ctrl-E` (line start/end), `Ctrl-B`/`Ctrl-F` and `←`/`→` (char),
`Alt-B`/`Alt-F` (word), `↑`/`↓` (line), `Ctrl-K`/`Ctrl-U` (kill to end/start),
`Ctrl-W` (delete word), `Ctrl-D` (delete forward), plus undo/redo. `Enter`
inserts a newline, `Ctrl-S` (or `Ctrl-Enter`) submits, `Esc` (or `Ctrl-C`)
cancels.

Every comment action is also a **clickable button** with its hotkey on the
label, so the mouse and keyboard reach the same actions. The focused diff line
floats a `comment(i)` button at its right edge (it acts on the current line or
multi-line selection); each thread box has a `reply(r)` / `resolve(R)` (and
`delete(D)` for a comment you added this session) row along its bottom; and the
composer shows `submit(ctrl+s)` / `cancel(esc)` below the input.

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

- `side`: `"new"` (added/context, RIGHT) or `"old"` (removed, LEFT).
- `range`: a single line uses `start == end`.
- `comments[0]` is the thread root; the rest are replies.
- `author`, `resolved`, `id`, `created_at` are optional (sensible defaults).
- A bare `[ ...threads... ]` array is also accepted.

**`id` is an opaque string, kept verbatim** — any string (a UUID, or a foreign id
such as a GitHub comment id). hew preserves it exactly, so the action log
references the same ids as the base and is replayable. A sidecar that omits `id`
gets a fresh one at load, so its actions won't match the on-disk base — fine for
ad-hoc viewing, not for replay.

## Action log format (output)

On exit hew prints a JSON **array of actions** to stdout — the minimal delta
turning the `--comments` base into the reviewed state. These four are the only
action types emitted:

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
  multi-line range (matching GitHub's `start_line`/`line` shape). Its `thread_id`
  is reused by any `reply` to the same thread.
- `reply` / `resolve` / `unresolve` reference an existing thread by `thread_id`
  (a base thread, or one `add_comment`-ed earlier in the log).
- `author` is omitted when unset; an untouched session prints `[]`.
- **There is no `delete` action.** Input threads can't be deleted; a thread
  created then removed in-session, or a resolve toggled back, leaves no trace.

## GitHub bridge

There is **no GitHub-specific code in hew** — the binary only speaks the JSON
above. The bridge is whoever consumes the log: an agent reads `gh api` PR threads
into a base sidecar, opens hew for the human, then replays the action log through
`gh`. Thin, convenience-only examples ship in [`examples/`](examples/):
`fetch_pr.sh` prepares the base sidecar from a PR, and `apply_actions.sh` replays
the log's `add_comment`s (reply/resolve need the consumer's `thread_id`→GitHub-id
mapping).

Real `{patch + comments}` pairs from public PRs also live in `examples/`:

```sh
hew examples/misskey-ja.patch   --comments examples/misskey-ja.comments.json
hew examples/rust-long-en.patch --comments examples/rust-long-en.comments.json
```

See [examples/README.md](examples/README.md) for how to fetch more.

## Design

`hew` stays intentionally small. These invariants keep it that way:

- **Never talks to GitHub.** It eats a patch + a comment JSON, nothing else. The
  GitHub round-trip lives outside the binary; any `gh` wrappers ship only as
  `examples/`, never as a dependency.
- **Pure filter — no "save".** All inputs are immutable: the patch (stdin) and
  the `--comments` base are read, never written. No save/autosave/document.
- **One in-memory store.** The TUI is its sole writer; all edits
  (compose/reply/resolve/delete) mutate one `CommentStore`.
- **Output is a compacted action log**, not the store: on exit hew emits
  `diff(base, final)` (cancellations included). Replay requires the base to carry
  stable thread ids.
- **Channels stay separated:** stdin = patch, stderr/tty = render, stdout =
  action-log result.
- **No daemon, no DB, no background services**, and a minimal CLI (just `FILE`
  and `--comments`).

Highlighting is via syntect over two-face's bat syntax set (broad language
coverage, pure-Rust fancy-regex).

**Planned:** a tree-sitter highlighting backend and theme selection. Live
in-session AI co-review over a socket is out of scope for v1 — the turn-based
flow (agent prepares a base review, human reviews in hew, agent drives `gh` from
the action log) covers the v1 workflows.

> **Note:** `hew` parses **plain unified diffs**, not git `format-patch` mailbox
> output (`gh pr diff --patch`). Use a `.diff` / `git diff` stream instead.
