// Convert GitHub PR review comments (pulls/.../comments) into hew's sidecar
// schema: { threads: [ { file, side, range:{start,end}, resolved, comments:[{author,body,created_at}] } ] }
import { readFileSync } from "node:fs";

const raw = JSON.parse(readFileSync(process.argv[2], "utf8"));

// Index by id so replies can find their root.
const byId = new Map(raw.map((c) => [c.id, c]));

function rootOf(c) {
  let cur = c;
  const seen = new Set();
  while (cur.in_reply_to_id != null && byId.has(cur.in_reply_to_id) && !seen.has(cur.id)) {
    seen.add(cur.id);
    cur = byId.get(cur.in_reply_to_id);
  }
  return cur;
}

// Group comments under their root id, preserving order.
const groups = new Map();
for (const c of raw) {
  const root = rootOf(c);
  if (!groups.has(root.id)) groups.set(root.id, []);
  groups.get(root.id).push(c);
}

const threads = [];
for (const [rootId, comments] of groups) {
  const root = byId.get(rootId);
  if (root.path == null) continue; // file-level / outdated without anchor
  const side = root.side === "LEFT" ? "old" : "new";
  // Multi-line comments carry start_line..line; single-line just line.
  const end = root.line ?? root.original_line;
  if (end == null) continue; // outdated comment with no current anchor
  const start = root.start_line ?? end;
  threads.push({
    file: root.path,
    side,
    range: { start, end },
    resolved: false,
    comments: comments.map((c) => ({
      author: c.user?.login ?? null,
      body: c.body,
      created_at: Date.parse(c.created_at) || Date.now(),
    })),
  });
}

process.stdout.write(JSON.stringify({ threads }, null, 2) + "\n");
