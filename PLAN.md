# hew — Plan

A high-performance, review-first terminal diff viewer (Rust).

> A from-scratch high-performance reimagining inspired by hunk (modem-dev/hunk). My own repo, built from zero.
>
> **Name origin**: *hew* = to cut/shape (a block) with an axe. Same "chunk/block" lineage as a diff `hunk`. Three letters — fastest possible binary name to type.

---

## 1. Goals / Non-Goals

### Goals
- A native single binary, review-first diff viewer with fast startup
- Open diff / show / patch / two-file comparisons in an interactive UI
- **Drive a running TUI** from an agent / CLI (the essential value of hunk)
- Implement **GitHub PR review comments as a real feature** locally (threads, ranges, resolve)
- Stay smooth on large diffs (viewport-lazy rendering)

### Non-Goals (stated explicitly to keep scope fixed)
- **No persistence** (everything in memory; gone when the window closes)
- **No GitHub / external-service integration** (hunk has none either)
- **No patch apply / edit / merge** (view + comment only; a read-only viewer)
- **No structural diff** (difftastic-style AST diff); line-based only
- jj support is out of initial scope (future)

---

## 2. Why build it

- hunk is slow: Node/Bun startup cost + eager, whole-changeset parse/highlight/layout for large diffs
- hunk's comments stop at "one-off sticky notes" → raise them to a **GitHub-PR-grade threaded comment feature**

---

## 3. Architecture

```
┌──────────────────────────── hew (single process) ────────────────────────┐
│                                                                           │
│  main thread: TUI render loop (ratatui + crossterm, synchronous)          │
│      ├─ poll terminal events (key/mouse)                                  │
│      └─ receive session commands via mpsc::Receiver → update → redraw     │
│                          ▲                                                │
│                          │ tokio::sync::mpsc / oneshot (responses)        │
│                          │                                                │
│  tokio task: session server (HTTP/JSON on 127.0.0.1, axum or hyper)       │
│      └─ accept JSON requests from the CLI, forward commands to main       │
│                                                                           │
└───────────────────────────────────────────────────────────────────────────┘
        ▲ HTTP (loopback)
        │
   hew session ...  (CLI / agent in another process)
```

### Bridging the synchronous TUI ↔ async server (the most important design point)
- The TUI is a synchronous render loop. It polls crossterm events with a **short timeout**, and on
  every loop drains the session-command channel (`mpsc::Receiver`) via `try_recv`.
- The session server (tokio task) wraps each request as a `SessionCommand` enum and sends it; the
  response comes back over a `oneshot::Sender` and is turned into an HTTP response.
- State (diff model + comments) is **solely owned by the TUI side**. The server only sends commands
  (no shared lock; one source of truth).

### Session discovery (picking among multiple TUIs)
- On startup each hew TUI registers `{session_id, repo_root, cwd, port}` with a **shared broker**
  (a separate resident process, or a registry on a known port). Same idea as hunk's session-broker.
- The CLI resolves the target (hunk-compatible):
  - `--repo <path>`: match by repo root (most common)
  - `<session-id>`: explicit
  - auto-resolve when only one session exists
- Start with the **minimal form**: no separate broker process — each TUI binds the next free port in
  a fixed range and writes itself into a registry file (`$XDG_RUNTIME_DIR/hew/sessions/*.json`),
  promoting to a resident broker only when needed.

---

## 4. Diff pipeline (separate generation from parsing)

| Input | How it's obtained | Notes |
|---|---|---|
| `hew diff <a> <b>` | read two files, **generate** with `similar` | text diff generation |
| `hew diff` (working tree) | fetch changes from git and diff | see §5 |
| `hew show [rev]` | fetch a commit's diff | see §5 |
| `hew patch -` / `<file>` | **parse** an existing unified diff | needs a parser (not generation) |

- **Generate**: `similar` (line/word-level diff, hunk grouping)
- **Parse**: a unified-diff parser. Candidates: `diffy` (can parse) or the `patch` crate; otherwise a thin homegrown parser.
- Both paths normalize to a shared `DiffFile { path, hunks: Vec<Hunk{ old_range, new_range, lines }> }`.

---

## 5. Git integration (risks and fallback)

- First choice: `gix` (pure Rust, fast, avoids subprocesses).
- **Risk**: gix's diff / blob-fetch APIs are immature in places.
- **Fallback policy** (staged):
  1. For the MVP, run `git`/`jj` as a **subprocess** for reliability (feed `git diff`/`git show` output through the §4 parser).
  2. Once benchmarks show I/O is the bottleneck, replace only the hot paths with `gix` (or `git2`).
- → "avoid subprocesses" is the **end goal**, not an MVP precondition (ship something that reliably works first).

---

## 6. Comment feature (GitHub-PR-review equivalent)

### Behaviors
| Feature | Behavior | Implementation |
|---|---|---|
| Line comment | attach to a specific line | `file + side + line` |
| Multi-line comment | select a range and attach | `range: [start, end]` |
| Thread / reply | replies hang off a root | tree via `parent_id` |
| Resolve / Unresolve | collapse a thread as resolved | per-thread `resolved` |
| Edit / delete | edit/delete your own comment | `edit` / `rm` |
| Author display | whose comment it is | `author` |
| Navigation | jump between comments | `next` / `prev` |

### Data model
Make the thread a first-class citizen (clarifies the unit of resolve/collapse):

```rust
struct Thread {
    id: Uuid,
    file: PathBuf,
    side: Side,            // Old | New
    range: LineRange,      // single line = start==end
    anchor: Anchor,        // anchor for reload resilience (§6.1)
    resolved: bool,        // per thread
    comments: Vec<Comment>,// [0] is the root, the rest are replies
}

struct Comment {
    id: Uuid,
    author: Option<String>,
    body: String,
    created_at: SystemTime,
}

enum Side { Old, New }
struct LineRange { start: u32, end: u32 }
```

- `Thread { comments }` makes the unit of resolve/render/reply more obvious than a `parent_id` scheme.
- **Drop `pending/submit` from the initial scope**: with no persistence and no external posting it has little meaning.
  If needed later, add it as a UI state ("finalize draft threads in a batch") — a view flag, not part of the model.

### 6.1 Anchoring / reload resilience (a core problem hunk also has)
- When the diff reloads under watch, line numbers shift and comments float loose.
- `Anchor` does not rely on line number alone: it holds `{ hunk_header_hint, context_line_text, offset_in_hunk }`
  and **re-anchors best-effort** after reload. If it can't match, flag it `orphaned` ("location unknown") rather than deleting it.
- For the MVP: start with "on reload, re-show comments by line number; mark them orphaned if they fall off," and improve accuracy later.

### Operations (two entry points)
- **In the TUI**: select a line/range → `c` comment / `r` reply / `R` toggle resolve / `e` edit / `d` delete / `n`/`N` jump
- **CLI / agent**: `hew session comment add | reply | resolve | edit | rm | list` + `hew session review --json` to export

---

## 7. Layout / UI

- **split** (old/new side-by-side) / **stack** (unified) / **auto** (switch by width) — like hunk.
- Sidebar for navigating between files.
- Mouse support (crossterm mouse events: click to select a line, drag to select a range, wheel to scroll).
- Wrap / line numbers / themes via config (optional, can come later).

---

## 8. Rust stack

| Area | Crate | Notes |
|---|---|---|
| TUI | `ratatui` + `crossterm` | rendering + key/mouse |
| diff generation | `similar` | from two texts/blobs |
| diff parsing | `diffy` or homegrown | unified patch parsing |
| highlight | **`syntect` first → `tree-sitter` later** | see §10 |
| CLI | `clap` (derive) | subcommands |
| session server | `tokio` + `axum` (lightweight HTTP/JSON) | loopback |
| async runtime | `tokio` | server + watch |
| git | subprocess → later `gix`/`git2` | §5 |
| watch | `notify` | `--watch` |
| JSON | `serde` + `serde_json` | |
| ID | `uuid` | |

External-integration crates (`octocrab`/`gh`) are **not used**.

---

## 9. Performance targets (make them measurable)

Benchmarks mirror hunk's bench layout so we can compare under the same conditions.

| Metric | Target | Measurement |
|---|---|---|
| startup → first frame (small diff) | < 50ms | bench: bootstrap |
| large diff (10k+ lines) first frame | < 200ms | bench: large-stream (viewport only) |
| one scroll frame | < 8ms (comfortably ~120fps) | bench: render-layout |
| highlight | viewport only + cache | bench: highlight |
| memory (large diff) | linear in input size, low constant | bench: memory |

Design principles:
- Do not lay out/highlight all lines eagerly. Compute **only the visible range + a small prefetch**.
- LRU-cache highlight results and formatted lines.
- Use ratatui's diff buffer to minimize redraw between frames.

---

## 10. Highlighting decision

- **MVP uses `syntect`**: a single crate + Sublime grammars; quick to adopt, broad language coverage.
- **`tree-sitter` later**: incremental parsing is strong for large files / live edits. In the perf-polish phase,
  swap in tree-sitter starting with whichever languages are the bottleneck.
- Abstract highlighting behind a trait (`Highlighter`) so the backend is swappable.

---

## 11. Project structure (proposed)

```
hew/
├─ Cargo.toml
├─ PLAN.md
├─ src/
│  ├─ main.rs            # clap entry, subcommand dispatch
│  ├─ cli.rs             # command definitions
│  ├─ diff/
│  │  ├─ model.rs        # DiffFile / Hunk / Line normalized representation
│  │  ├─ generate.rs     # similar-based generation
│  │  └─ parse.rs        # unified patch parsing
│  ├─ vcs/
│  │  └─ git.rs          # diff/show (subprocess at first)
│  ├─ comments/
│  │  ├─ model.rs        # Thread / Comment / Anchor
│  │  └─ anchor.rs       # re-anchor on reload
│  ├─ session/
│  │  ├─ server.rs       # axum loopback JSON API
│  │  ├─ protocol.rs     # request/response + SessionCommand
│  │  └─ registry.rs     # session discovery (registry file)
│  ├─ ui/
│  │  ├─ app.rs          # state + render loop + event/command handling
│  │  ├─ layout.rs       # split/stack/auto
│  │  ├─ diff_pane.rs
│  │  ├─ comment_view.rs
│  │  └─ highlight.rs    # Highlighter trait + syntect impl
│  └─ watch.rs           # notify
└─ tests/                # integration tests
```

---

## 12. Testing strategy

- **Unit**: diff parsing (each unified-patch shape), diff generation, comment/thread model, re-anchoring, session protocol round-trip.
- **Golden/snapshot**: pin layout (split/stack) output with `insta`.
- **Integration**: stand up the server and verify the `hew session ...` JSON contract (add comment → it shows in review --json, etc.).
- **Bench**: §9 via `criterion` or a homegrown harness; regression detection in CI.

---

## 13. Milestones (with acceptance criteria)

1. **Static diff display** — parse/generate `hew patch -` / `hew diff <a> <b>` and show in ratatui (no color), with scrolling.
   _Done when_: a fairly large patch opens, scrolling is smooth, no crashes.
2. **Git support** — `hew diff` (working tree) / `hew show [rev]` (subprocess at first).
   _Done when_: diff/show open on a real repo.
3. **Session foundation** — loopback server + registry + `hew session list / review --json / navigate`.
   _Done when_: a CLI in another process can navigate a running TUI.
4. **Comments (line/single)** — `hew session comment add / list` → inline rendering + `c` in the TUI.
   _Done when_: comments attach from both the CLI and the TUI and appear in review --json.
5. **PR-style comments** — range / thread (`reply`) / `resolve` / `edit` / `rm` + collapse UI + `n/N` jump.
   _Done when_: thread reply/resolve/collapse work from both the TUI and the CLI.
6. **Performance polish** — syntect highlighting + viewport laziness + caching; hit §9 targets. Partial swap to tree-sitter / gix if needed.
   _Done when_: §9 bench targets are met and startup/large-diff are clearly faster than hunk.

---

## 14. Key risks

| Risk | Mitigation |
|---|---|
| Bridging the sync TUI and async server is complex | restrict to mpsc + oneshot; TUI solely owns state (§3) |
| gix's diff/blob is immature | subprocess for the MVP, swap only hot paths later (§5) |
| comments float loose on reload | re-anchor via Anchor + orphaned display (§6.1) |
| syntect is slow on large files | viewport only + cache; tree-sitter as last resort (§10) |

---

## 15. Open questions

- Make the session protocol **hunk-compatible JSON** or our own (compatibility lets us reuse hunk's skill/agent workflows).
- Keep the registry as a file-based scheme or promote it to a resident broker (depends on load).
- When to introduce a config format (toml) and a theming mechanism.
