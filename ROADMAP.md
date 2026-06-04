# hew roadmap

From **read-only patch viewer** → **review buffer / pure filter**: a human reviews
a diff in the TUI (composing, replying, resolving), and on exit hew emits the
session's changes as a compacted **action log** to stdout — which an agent turns
into GitHub review actions, or feeds into the next step.

## North star (v1, turn-based — no socket)

```
# agent prepares a base review (its own comments, or fetched PR threads),
# opens hew for the human, then drives gh from the action log:
agent-review > base.json
git diff | hew --comments base.json > actions.json
  ├─ human reads the agent's comments, replies (r), resolves (R), adds (i)
  └─ q → actions.json = compacted action log (delta vs the immutable base)
agent applies actions.json  (post to GitHub / fix code / …)
```

Live in-session AI co-review over a socket is **deferred** (see the Deferred
section); the turn-based flow above covers the v1 workflows without it.

## Design invariants (do not break — these are what keep hew *not* fat)

- [ ] **hew never talks to GitHub.** It eats a patch + a comment JSON, nothing
      else. GitHub round-trip is external `gh` wrappers (shipped only as
      `examples/`, never a dependency).
- [ ] **hew is a pure filter — no "save".** All inputs are immutable: the patch
      (stdin) and the `--comments` base JSON are read, never written. There is
      no save/flush/autosave/document concept.
- [ ] **The TUI is the sole writer of its in-memory store.** All edits
      (compose/reply/resolve/delete) mutate one `CommentStore`; nothing else
      writes it during a session.
- [ ] **Output is a compacted action log**, not the comment store. On exit hew
      emits `diff(base, final)` as a minimal action array to **stdout** (a
      thread created then deleted, or a resolve toggled back, cancels out).
      Replaying the log against the base requires the base to carry **stable
      thread ids**; an id-less sidecar gets random ids at load and isn't
      replayable (fine for ad-hoc viewing).
- [ ] **Channels stay separated:** stdin = patch, stderr/tty = render,
      stdout = action-log result.
- [ ] **No daemon, no DB, no background services** in v1.
- [ ] Resist new flags. The CLI stays minimal: just `FILE` and `--comments`.

## Flag changes

- [x] `--comments` is an **immutable input** (load-only, the review's starting
      base). hew never writes back to it; output is the action log on stdout.
- [x] Removed `--json` (parsed-changeset dump — unrelated to the review output).
- [x] Removed `--watch`. It only reloaded a file-input patch (not the common
      stdin pipe), contradicted the turn-based flow (you fix *after* reviewing),
      and reintroduced comment-anchor drift on reload. The patch is fixed for
      the session.

---

## Phase 1 — Channel hygiene (foundation, ships standalone) ✅

Frees stdout so the review JSON can be the program's result, fzf-style.

- [x] Move TUI rendering from `stdout` → `stderr` (`CrosstermBackend::new(stderr())`)
      in `src/ui/mod.rs`.
- [x] Move OSC 52 clipboard writes from stdout → stderr in `src/ui/app.rs`.
- [x] Verify the existing tty-borrow logic (`reattach_stdin_to_tty`) still holds
      with stderr as the render target (macOS `/dev/tty` + kqueue `EINVAL` caveat).
- [x] Build + test suite green.
- [x] `git diff | hew` renders correctly on a real terminal.
- [x] `git diff | hew > out.json` renders to the terminal (not the file) and
      leaves the terminal clean on exit.

## Phase 2 — Output model: compacted action log ✅

hew is a filter: immutable inputs in, an action log out. (Originally drafted as a
"persistence round-trip" that wrote back to `--comments`; that was wrong — it
broke input immutability and smuggled in an editor-style "save" concept. Now the
output is a delta, never a write-back.)

- [x] `--comments <file>` loads the **immutable** base (empty when absent);
      hew never writes to it.
- [x] On exit, emit `comments::diff(base, final)` — the minimal action array
      (`add_comment` / `reply` / `resolve` / `unresolve` / `delete`) — to
      **stdout**, always (an untouched session prints `[]`).
- [x] Compaction falls out of diffing: add-then-delete and resolve-then-unresolve
      cancel; redundant toggles collapse to net effect. Unit-tested + missing-file
      load test.

## Phase 3 — In-app comment authoring (TUI becomes writable)

hew started life read-only. This phase makes the store writable: TUI edits
mutate **one** in-memory `CommentStore` through a single shared write path.

- [x] Add mutation methods to `CommentStore` (`add_thread`, `reply`,
      `remove_thread`, `toggle_resolved`) — the single shared write path,
      unit-tested, all driven by the TUI.
- [x] Make the loaded `CommentStore` owned mutably by the running app/TUI.
- [x] Remove a thread from the TUI (`D`).
- [x] **Resolve / unresolve** a thread from the TUI (`R`, toggles
      `Thread.resolved`).
- [x] Re-render after every mutation via `rebuild_rows` (anchor-preserving;
      cursor/scroll stay stable).
- [x] Comment composer in the TUI (`i`): modal input box, write a body, submit
      → new thread on the current line (author `you`).
- [x] Reply to an existing thread from the TUI (`r`).
- [ ] Visual line-select mode (`v`) to anchor a comment to a multi-line
      `(file, side, range)` (composer currently anchors to the single cursor
      line).
- [ ] Multi-line comment bodies (composer is single-line; `enter` submits).
- [ ] Visually distinguish resolved threads beyond the header flag (dim /
      collapsed / filterable).

## Phase 4 — GitHub bridge (examples only, never in the binary)

The v1 workflow is turn-based and needs no socket: an agent writes its review to
a base JSON, opens `hew --comments base.json` for the human, and reads the
action log from stdout on exit to drive `gh`. This phase is just the example
wrappers.

- [ ] Shape `Thread`/`Comment` JSON close to a GitHub review-thread structure so
      translation stays trivial (path/side/line/resolved/body/author/created_at,
      stable thread id for re-post matching).
- [ ] `examples/gh-to-hew` — `gh api` PR threads → hew comment JSON.
- [ ] `examples/hew-to-gh` — hew JSON → posted GitHub review/thread drafts.
- [ ] Handle line anchoring (GitHub `(path, line, side, commit)` ↔ hew
      `(file, LineRange, side)`) inside the bridge, not in hew.
- [ ] README example of the full PR review loop.

## Deferred — live socket co-review (post-v1)

A running TUI advertising a Unix socket so an agent can inject comments and read
responses **live**, without closing hew. Implemented once (session registry +
`hew comment list` over an mpsc-forwarded socket, PR #7) then **removed to keep
v1 lean** — the turn-based flow (base JSON in, action log out) covers the
intended "review before PR" and "review PR #N" workflows without it. Revisit only
if live, multi-turn co-review in a single session becomes a real need; it also
requires solving anchor-drift when the patch reloads mid-session.

- [ ] Session registry + socket listener (recover from PR #7 history).
- [ ] `hew comment add/remove/resolve/list` client subcommands over the socket.
- [ ] `hew sessions` enumeration + multi-session target resolution.
- [ ] Anchor re-mapping when the patch reloads under a live session.

## Open questions

- [ ] `--comments` vs `--review` naming.
- [ ] Anchor remapping when the patch changes is **moot in v1**: the patch is
      fixed for the session (no `--watch`). It only resurfaces if live patch
      reload (Deferred) is ever added.
