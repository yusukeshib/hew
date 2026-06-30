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

/// Shown for bare `hew` when stdin is interactive and there is no patch to
/// review. Keep this terse; `hew --help` is the full agent-oriented manual.
pub const SHORT_HELP: &str = "\
review-first terminal patch viewer

Usage: hew [OPTIONS] [FILE]

Arguments:
  [FILE]  Patch file to review. Omit or use `-` to read from stdin.

Options:
      --comments <FILE>  Load existing review comments from a sidecar JSON file (immutable input)
  -h, --help             Print help (the full agent-oriented manual)
  -V, --version          Print version
";

/// Shown on `--help`. Written so an agent can drive hew from `hew --help`
/// alone: the channel contract, the turn-based workflow, and the input/output
/// JSON schemas.
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

LLM / AGENT QUICK START:
  - Feed hew a plain unified diff (`git diff`, `git diff --staged`, or
    `gh pr diff`). Do not feed format-patch mailbox output.
  - Put your draft inline review threads in --comments JSON before launching.
  - Let the human edit/resolve/add comments in the TUI.
  - After hew exits, parse stdout as JSON. Treat it as commands to apply to
    your original base: add_comment, reply, resolve, unresolve.
  - If stdout is [], the human made no changes. Do not infer approval unless
    your surrounding workflow says so.
  - Keep stderr/tty separate from stdout; stdout is the only machine contract.

INPUT — comment sidecar (--comments), an immutable base:
  {\"threads\":[{\"id\":\"<id>\",\"file\":\"src/x.rs\",\"side\":\"new\",
   \"range\":{\"start\":10,\"end\":12},\"resolved\":false,
   \"comments\":[{\"author\":\"agent\",\"body\":\"...\"}]}]}
  - side: \"new\" (RIGHT) or \"old\" (LEFT); a single line uses start == end.
  - id/author/resolved/created_at are optional. `id` is an opaque string
    (a UUID, or a foreign id such as a GitHub comment id) kept VERBATIM, so the
    action log is replayable against this base. A missing id gets a fresh one at
    load (fine for viewing, but not replayable — give stable ids for replay).
  - A bare [ ...threads... ] array is also accepted.

OUTPUT — action log (stdout), the minimal delta turning base into reviewed:
  [{\"action\":\"add_comment\",\"thread_id\":\"<id>\",\"file\":\"src/x.rs\",
    \"side\":\"new\",\"start_line\":10,\"line\":12,\"body\":\"...\",\"author\":\"you\"},
   {\"action\":\"reply\",\"thread_id\":\"<id>\",\"body\":\"...\"},
   {\"action\":\"resolve\",\"thread_id\":\"<id>\"},
   {\"action\":\"unresolve\",\"thread_id\":\"<id>\"}]
  - These four are the only action types; there is no `delete` action.
  - add_comment's `line` is the thread's last line; `start_line` appears only
    for a multi-line range (omitted for a single line) — GitHub's shape.
  - reply/resolve/unresolve reference a thread by thread_id, an opaque
    string echoing the base id verbatim (a base thread, or one add_comment-ed
    earlier in the same log).
  - Compaction is automatic: a thread created then removed in-session, or a
    resolve toggled back, leaves no trace in the log.

Note: hew parses plain unified diffs, not git format-patch mailbox output.";

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn bare_help_is_short() {
        assert!(SHORT_HELP.contains("Usage: hew [OPTIONS] [FILE]"));
        assert!(!SHORT_HELP.contains("CHANNELS"));
        assert!(!SHORT_HELP.contains("TURN-BASED WORKFLOW"));
    }

    #[test]
    fn clap_help_is_long_manual() {
        let mut help = Vec::new();
        Cli::command().write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();
        assert!(help.contains("CHANNELS"));
        assert!(help.contains("TURN-BASED WORKFLOW"));
        assert!(help.contains("INPUT — comment sidecar"));
    }
}
