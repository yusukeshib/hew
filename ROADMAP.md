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

Live in-session AI co-review over a socket is out of scope for v1; the
turn-based flow above covers the v1 workflows without it.

## Design invariants (do not break — these are what keep hew *not* fat)

- [ ] **hew never talks to GitHub.** It eats a patch + a comment JSON, nothing
      else. The GitHub round-trip lives entirely outside the binary: an agent
      that knows the documented JSON schema is the bridge (any `gh` wrappers are
      shipped only as `examples/`, never a dependency).
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
- [x] Visual line-select mode (`v`) to anchor a comment to a multi-line
      `(file, side, range)`. Extend the selection with `j`/`k` (or a mouse
      drag), then `i` anchors the new thread to the spanned range; the range
      surfaces in the action log's `start_line`/`line`.
- [x] Multi-line comment bodies: `enter` inserts a newline, `ctrl-s` submits.
      Bodies round-trip through `\n` and render across multiple box lines.
- [x] Visually distinguish resolved threads beyond the header flag: the whole
      thread box (border + author + body) dims to `muted` when resolved.

## Phase 4 — Agent-friendly contract (the "bridge" is the schema)

The GitHub round-trip needs **no code in (or shipped with) hew**. hew is a pure
filter with a documented JSON contract; an agent that understands the schema *is*
the bridge — it reads `gh api` PR threads into a base sidecar, opens
`hew --comments base.json` for the human, then replays the action log through
`gh`. So this phase is about making that contract legible to an agent, not about
writing wrapper scripts.

- [x] Document the **input** sidecar schema and the **output** action-log schema
      (the five actions, `thread_id` reuse, `author` omission, `[]`, automatic
      compaction) in the README, framed for an agent consumer.
- [x] Make `hew --help` self-describing for agents: channel contract
      (stdin=patch, stdout=action log, stderr/tty=render), the turn-based
      workflow, and the input/output schemas inline — so `hew --help` alone is
      enough to drive it (`-h` stays terse via clap's `long_about`).
- [x] Keep `Thread`/`Comment`/`Action` JSON shaped close to a GitHub
      review-thread structure (path/side/line/resolved/body/author/created_at,
      **stable thread id** for re-post matching) so an agent's translation stays
      trivial. Decided: `add_comment` carries the full range as GitHub's
      `start_line`/`line` pair — `line` is the thread's last line, `start_line`
      appears only for a multi-line range (omitted for single line). No longer
      drops `range.end`.
- [x] (Optional) Ship thin `gh` one-liner examples in `examples/` as a
      convenience — never a dependency, never in the binary.
      `examples/fetch_pr.sh` prepares the base sidecar from a PR;
      `examples/apply_actions.sh` replays the action log's `add_comment`s via
      `gh` (reply/resolve/delete need the consumer's id mapping).

## Open questions

- [ ] `--comments` vs `--review` naming.
- [ ] Anchor remapping when the patch changes is **moot in v1**: the patch is
      fixed for the session (no `--watch`). It only resurfaces if live patch
      reload is ever added.
