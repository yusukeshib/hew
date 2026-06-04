# hew roadmap

From **read-only patch viewer** → **two-way review buffer** where humans (TUI)
and AI agents (`hew comment …` over a socket) discuss inline, and the structured
result is flushed to JSON on exit — usable as a GitHub PR thread draft *and* as
input for the next AI session.

## North star

```
git diff | hew --comments review.json
  ├─ human types in the TUI         (v → select, i → comment)
  ├─ AI talks to the live process   (hew comment add --reply-to …)
  └─ q → flush review.json          (GitHub draft / next AI session input)
```

## Design invariants (do not break — these are what keep hew *not* fat)

- [ ] **hew never talks to GitHub.** It eats a patch + a comment JSON, nothing
      else. GitHub round-trip is external `gh` wrappers (shipped only as
      `examples/`, never a dependency).
- [ ] **The running TUI process is the sole writer of the comment store.** Human
      input *and* AI `comment add` both mutate one in-memory `CommentStore`.
      No two writers, no file-edit races.
- [ ] **No daemon, no DB.** Multi-process coordination is a registry directory of
      sockets + tiny JSON metadata. Each process registers itself and cleans up.
- [ ] **Sessions never talk to each other.** Cross-session is the *client's* job
      (read the registry). Each hew is autonomous.
- [ ] **hew is a pure filter — no "save".** All inputs are immutable: the patch
      (stdin) and the `--comments` base JSON are read, never written. There is
      no save/flush/autosave/document concept.
- [ ] **Output is a compacted action log**, not the comment store. On exit hew
      emits `diff(base, final)` as a minimal action array to **stdout** (a
      thread created then deleted, or a resolve toggled back, cancels out).
      Replaying the log against the base requires the base to carry **stable
      thread ids**; an id-less sidecar gets random ids at load and isn't
      replayable (fine for ad-hoc viewing).
- [ ] **Channels stay separated:** stdin = patch, stderr/tty = render,
      stdout = action-log result.
- [ ] Resist new flags. Behaviour should be implicit (always listen, auto-watch),
      not opt-in. `--name` is the only genuinely new flag.

## Flag changes

- [ ] Remove `--watch` for comments (replaced by socket IPC; keeping it would
      clobber the in-memory store on reload).
- [ ] Decide patch reload: make patch auto-reload default-on for file inputs
      (mirror of "always listen"), dropping the `--watch` flag entirely.
- [ ] Drop `--listen` idea — listening is always-on for any TUI session.
- [x] `--comments` is an **immutable input** (load-only, the review's starting
      base). hew never writes back to it; output is the action log on stdout.
- [ ] Add `--name <id>` (optional) to label a session in the registry.

---

## Phase 1 — Channel hygiene (foundation, ships standalone) ✅

Frees stdout so the review JSON can be the program's result, fzf-style.

- [x] Move TUI rendering from `stdout` → `stderr` (`CrosstermBackend::new(stderr())`)
      in `src/ui/mod.rs`.
- [x] Move OSC 52 clipboard writes from stdout → stderr in `src/ui/app.rs`.
- [x] Verify the existing tty-borrow logic (`reattach_stdin_to_tty`) still holds
      with stderr as the render target (macOS `/dev/tty` + kqueue `EINVAL` caveat).
- [x] Build + test suite green; `--json` still writes the changeset to stdout.
- [ ] Manual check: `git diff | hew` renders correctly on a real terminal.
- [ ] Manual check: `git diff | hew > out.json` renders to the terminal (not the
      file) and leaves the terminal clean on exit.

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

hew started life read-only. This phase makes the store writable: the TUI and
(later) the socket mutate **one** in-memory `CommentStore`, so the mutation API
lands here first and Phase 4's socket rides on it.

- [x] Add mutation methods to `CommentStore` (`add_thread`, `reply`,
      `remove_thread`, `set_resolved`/`toggle_resolved`) — the single shared
      write path, unit-tested. (`add_thread`/`reply`/`set_resolved` wired up by
      the composer + Phase 4 socket; `#[allow(dead_code)]` until then.)
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
- [ ] Mark the store dirty so Phase 4 flush knows there is something to save.

## Phase 4 — Live editing over a socket (AI joins the discussion)

**Slice 1 (read path) ✅**

- [x] On TUI start, register a session: create
      `$XDG_RUNTIME_DIR/hew/<id>.sock` (+ `<id>.json` metadata: id, pid, cwd,
      name, files[]). Fall back to `/tmp/hew-$UID/`.
- [x] Listen on the socket on a daemon thread; forward each request to the main
      loop over an `mpsc` channel (keeps the TUI the sole store writer — no
      shared lock in the render path).
- [x] Clean up socket + metadata on exit (`Session` drop); sweep stale entries
      (dead pid via `kill(pid, 0)`) on startup.
- [x] `hew comment list [--session <id>]` → dump the store as JSON.
- [x] Target resolution: `--session` (id/name) → only-live-session → error.
- [x] Socket round-trip + discovery + cleanup integration-tested.

**Slice 2 (write path)**

- [ ] Forward writes through the same channel and re-render after applying:
- [ ] `hew comment add --file <p> --line <n> [--side old|new] --body <s> [--reply-to <id>]`
- [ ] `hew comment remove <comment-id|thread-id>`
- [ ] `hew comment resolve <thread-id>` / `hew comment unresolve <thread-id>`
      (so an AI can close out a thread once addressed).
- [ ] Re-render the TUI promptly when a socket write lands (event wakeup).

## Phase 5 — Multi-session addressing

- [ ] `hew sessions [--json]` → enumerate the registry (name, cwd, repo, ref,
      files, thread count).
- [ ] Client target resolution order:
      1. [ ] `--session <name>` explicit
      2. [ ] exactly one live session → auto
      3. [ ] multiple, but only one contains the `--file` → auto-route via metadata
      4. [ ] otherwise → error listing candidates, require `--session`
- [ ] Friendly error output when the target is ambiguous.

## Phase 6 — GitHub bridge (examples only, never in the binary)

- [ ] Shape `Thread`/`Comment` JSON close to a GitHub review-thread structure so
      translation stays trivial (path/side/line/resolved/body/author/created_at,
      stable thread id for re-post matching).
- [ ] `examples/gh-to-hew` — `gh api` PR threads → hew comment JSON.
- [ ] `examples/hew-to-gh` — hew JSON → posted GitHub review/thread drafts.
- [ ] Handle line anchoring (GitHub `(path, line, side, commit)` ↔ hew
      `(file, LineRange, side)`) inside the bridge, not in hew.
- [ ] README example of the full PR review loop.

## Open questions

- [ ] `--comments` vs `--review` naming.
- [ ] Anchor remapping when the patch reloads and hunk line numbers shift
      (pre-existing issue, but the two-way flow makes it more visible).
- [ ] Conflict/merge policy if `comment add` targets a line that no longer exists
      after a patch reload.
- [ ] Windows: the socket layer is Unix-domain; decide whether Windows is out of
      scope or needs a named-pipe shim.
