//! Command-line interface (clap).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "hew",
    version,
    about = "review-first terminal patch viewer",
    long_about = LONG_ABOUT
)]
pub struct Cli {
    /// Patch file to review. Omit or use `-` to read from stdin.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Load existing review comments from a sidecar JSON file (immutable input).
    #[arg(long, value_name = "FILE")]
    pub comments: Option<PathBuf>,
}

/// Shown on `--help` (terse `about` stays on `-h`). Written so an agent can
/// drive hew from `hew --help` alone: the channel contract, the turn-based
/// workflow, and the input/output JSON schemas.
const LONG_ABOUT: &str = "\
review-first terminal patch viewer

hew is a pure filter. It reads a unified diff and an optional immutable comment
sidecar, opens an interactive review TUI, and on exit prints a compacted action
log (the review delta) to stdout. It never talks to GitHub and never writes its
inputs.

CHANNELS (kept separate so the output is machine-consumable):
  stdin       unified diff (patch) to review
  stdout      JSON action log (the program's result; `[]` when untouched)
  stderr/tty  the interactive TUI render

TURN-BASED WORKFLOW (the \"bridge\" to GitHub is an agent, not hew):
  # agent builds a base review (its own, or fetched PR threads), opens hew for
  # the human, then drives `gh` from the emitted action log:
  agent-review > base.json
  git diff | hew --comments base.json > actions.json
  agent applies actions.json   # post to GitHub / fix code / next step

INPUT — comment sidecar (--comments), an immutable base:
  {\"threads\":[{\"id\":\"<uuid>\",\"file\":\"src/x.rs\",\"side\":\"new\",
   \"range\":{\"start\":10,\"end\":12},\"resolved\":false,
   \"comments\":[{\"author\":\"agent\",\"body\":\"...\"}]}]}
  - side: \"new\" (RIGHT) or \"old\" (LEFT); a single line uses start == end.
  - id/author/resolved/created_at are optional. Replay needs STABLE ids: an
    id-less sidecar gets random ids at load and is not replayable.
  - A bare [ ...threads... ] array is also accepted.

OUTPUT — action log (stdout), the minimal delta turning base into reviewed:
  [{\"action\":\"add_comment\",\"thread_id\":\"<uuid>\",\"file\":\"src/x.rs\",
    \"side\":\"new\",\"line\":10,\"body\":\"...\",\"author\":\"you\"},
   {\"action\":\"reply\",\"thread_id\":\"<uuid>\",\"body\":\"...\"},
   {\"action\":\"resolve\",\"thread_id\":\"<uuid>\"},
   {\"action\":\"unresolve\",\"thread_id\":\"<uuid>\"},
   {\"action\":\"delete\",\"thread_id\":\"<uuid>\"}]
  - reply/resolve/unresolve/delete reference a thread by thread_id (a base
    thread, or one add_comment-ed earlier in the same log).
  - Compaction is automatic: add-then-delete and resolve-then-unresolve cancel.

Note: hew parses plain unified diffs, not git format-patch mailbox output.";
