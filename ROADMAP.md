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
- [ ] **load target == flush target.** `--comments file` opens *and* saves to the
      same file (like opening a file in an editor).
- [ ] **Channels stay separated:** stdin = patch, stderr/tty = render,
      stdout = JSON result.
- [ ] Resist new flags. Behaviour should be implicit (always listen, auto-watch),
      not opt-in. `--name` is the only genuinely new flag.

## Flag changes

- [ ] Remove `--watch` for comments (replaced by socket IPC; keeping it would
      clobber the in-memory store on reload).
- [ ] Decide patch reload: make patch auto-reload default-on for file inputs
      (mirror of "always listen"), dropping the `--watch` flag entirely.
- [ ] Drop `--listen` idea — listening is always-on for any TUI session.
- [ ] Promote `--comments` from "read-only sidecar" to "edited review document"
      (load if present, else start empty; flush to it on exit). Consider rename
      to `--review` (or keep `--comments` for familiarity — TBD).
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

## Phase 2 — Persistence round-trip (`--comments` as a document) ✅

Output is dead unless it can be re-loaded; this closes the loop. (Authoring lands
in Phase 3; this phase just wires load+flush so authored comments survive.)

- [x] On exit, flush the in-memory `CommentStore` as JSON:
      - [x] to `--comments <file>` when given (same file it loaded from)
      - [x] to **stdout** when `--comments` is omitted
- [x] `--comments <file>` loads existing state when the file exists, starts empty
      when it doesn't (`load_comments_or_default`).
- [x] Omitted-`--comments` exit: stdout **only when the store is non-empty**, so a
      plain `git diff | hew` view never prints an empty `{ "threads": [] }`. No
      auto-`.hew/` file — kept explicit.
- [x] Round-trip test (`save_then_load_roundtrips`) + missing-file test. (The
      full open→author→quit→reopen loop completes once Phase 3 adds authoring.)

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
- [ ] Visual line-select mode (`v`) to anchor a comment to a `(file, side, range)`.
- [ ] Comment composer in the TUI (`i`): open an input box, write a body, submit
      → new thread on the selection. **(next PR)**
- [ ] Reply to an existing thread from the TUI. **(next PR)**
- [ ] Visually distinguish resolved threads beyond the header flag (dim /
      collapsed / filterable).
- [ ] Mark the store dirty so Phase 4 flush knows there is something to save.

## Phase 4 — Live editing over a socket (AI joins the discussion)

- [ ] On TUI start, register a session: create
      `$XDG_RUNTIME_DIR/hew/<id>.sock` (+ `<id>.json` metadata: pid, cwd, repo,
      target ref, files[], thread count). Fall back to `/tmp/hew-$UID/`.
- [ ] Listen on the socket; on connect, accept one line of command, apply to the
      in-memory store, trigger a re-render.
- [ ] Clean up socket + metadata on exit; sweep stale sockets (unconnectable
      `.sock`) on startup.
- [ ] `hew comment add --file <p> --line <n> [--side old|new] --body <s> [--reply-to <id>]`
- [ ] `hew comment list` → dump current store as JSON (so an AI can read state).
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
