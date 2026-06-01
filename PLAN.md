# hew — Plan

高性能 review-first ターミナル diff ビューア (Rust)。

> hunk (modem-dev/hunk) にインスパイアされた自作の高性能版。ゼロから作る自分のリポジトリ。
>
> **名前の由来**: *hew* = 斧などで（塊を）切り出す。diff の `hunk`(塊) と同根のニュアンス。3文字でバイナリ名として最速。

---

## 1. ゴール / Non-Goals

### ゴール
- ネイティブ単一バイナリの、起動が速い review-first diff ビューア
- diff / show / patch / 2ファイル比較を対話 UI で開く
- エージェント / CLI から **稼働中の TUI を操作**できる（hunk の本質的価値）
- GitHub PR レビュー**コメント機能そのもの**をローカルに実装（スレッド・範囲・resolve）
- 大 diff でもカクつかない（ビューポート遅延描画）

### Non-Goals（やらないことを明示してスコープ固定）
- **永続化しない**（全部メモリ。ウィンドウを閉じたら消える）
- **GitHub / 外部サービス連携しない**（hunk にも無い）
- **patch の適用・編集・マージしない**（閲覧とコメントのみ。read-only ビューア）
- **構造的 diff（difftastic 的 AST diff）はしない**（行ベース）
- jj 対応は初期スコープ外（将来）

---

## 2. なぜ作るか

- hunk が遅い: Node/Bun の起動コスト + 大 diff のパース/ハイライト/レイアウトが事前一括計算
- hunk のコメントは「単発の付箋」止まり → GitHub PR 並みの**スレッド型コメント機能**に引き上げたい

---

## 3. アーキテクチャ

```
┌──────────────────────────── hew (1 プロセス) ─────────────────────────────┐
│                                                                           │
│  main thread: TUI レンダループ (ratatui + crossterm, 同期)                │
│      ├─ terminal events (key/mouse) を poll                               │
│      └─ session command を mpsc::Receiver で受信 → 状態更新 → 再描画       │
│                          ▲                                                │
│                          │ tokio::sync::mpsc / oneshot (応答)             │
│                          │                                                │
│  tokio task: session server (127.0.0.1 上の HTTP/JSON, axum or hyper)     │
│      └─ CLI からの JSON リクエストを受け、command を main thread へ送る    │
│                                                                           │
└───────────────────────────────────────────────────────────────────────────┘
        ▲ HTTP (loopback)
        │
   hew session ...  (別プロセスの CLI / エージェント)
```

### 同期 TUI ↔ 非同期 server の橋渡し（最重要設計）
- TUI は同期レンダループ。crossterm の event を**短いタイムアウトで poll**しつつ、
  毎ループで session command チャネル(`mpsc::Receiver`)を `try_recv` で吸う。
- session server（tokio task）はリクエストを `SessionCommand` enum にして送り、
  応答は `oneshot::Sender` で受け取って HTTP レスポンスに変換。
- 状態（diff モデル + コメント）は **TUI 側に単一所有**。server はコマンドを送るだけ
  （共有ロックを避け、状態の真実は1か所）。

### session 発見（複数 TUI の選択）
- 各 hew TUI は起動時に **共有ブローカ**（別の常駐プロセス or 既定ポートのレジストリ）へ
  `{session_id, repo_root, cwd, port}` を登録。hunk の session-broker 同様。
- CLI は以下で対象を解決（hunk 互換）:
  - `--repo <path>`: repo root マッチ（最頻）
  - `<session-id>`: 明示
  - セッションが1つなら自動解決
- まず**最小構成**: ブローカを別プロセスにせず、各 TUI が固定ポート範囲を順に bind し、
  レジストリファイル（`$XDG_RUNTIME_DIR/hew/sessions/*.json`）に自分を書く方式から始め、
  必要になったら常駐ブローカへ昇格。

---

## 4. diff パイプライン（生成と解析を分離）

| 入力 | 取得方法 | 備考 |
|---|---|---|
| `hew diff <a> <b>` | 2ファイルを読み `similar` で **生成** | テキスト diff 生成 |
| `hew diff`（作業ツリー） | git から変更を取得し diff | §5 参照 |
| `hew show [rev]` | コミットの diff を取得 | §5 参照 |
| `hew patch -` / `<file>` | 既存 unified diff を **解析** | パーサが必要（生成ではない） |

- **生成**: `similar`（行/語単位 diff、hunk 化）
- **解析**: unified diff パーサ。候補 `diffy`（解析可）or `patch` クレート、無ければ薄い自前パーサ。
- 内部表現は両経路で共通の `DiffFile { path, hunks: Vec<Hunk{ old_range, new_range, lines }> }` に正規化。

---

## 5. git 連携（リスクとフォールバック）

- 第一候補 `gix`（純 Rust、高速、サブプロセス回避）。
- **リスク**: gix の diff / blob 取得 API は領域により未成熟。
- **フォールバック方針**（段階）:
  1. MVP では `git`/`jj` を確実性優先で **サブプロセス起動**（`git diff`/`git show` の出力を §4 パーサに通す）。
  2. ベンチで I/O がボトルネックと判明したら、ホットパスのみ `gix`（or `git2`）に置換。
- → 「サブプロセス回避」は**最終目標**であって MVP の前提にしない（確実に動くものを先に）。

---

## 6. コメント機能（GitHub PR レビュー相当）

### 振る舞い
| 機能 | 挙動 | 実装 |
|---|---|---|
| 行コメント | 特定行に付ける | `file + side + line` |
| 複数行コメント | 範囲選択して付ける | `range: [start, end]` |
| スレッド / 返信 | reply がぶら下がる | `parent_id` でツリー |
| Resolve / Unresolve | スレッドを解決済みに畳む | スレッド単位 `resolved` |
| 編集 / 削除 | 自分のコメントを編集/削除 | `edit` / `rm` |
| author 表示 | 誰のコメントか | `author` |
| ナビゲーション | コメント間ジャンプ | `next` / `prev` |

### データモデル
スレッドを一級市民にする（resolve/折りたたみの単位を明確化）:

```rust
struct Thread {
    id: Uuid,
    file: PathBuf,
    side: Side,            // Old | New
    range: LineRange,      // 単一行 = start==end
    anchor: Anchor,        // reload 耐性のためのアンカー（§6.1）
    resolved: bool,        // スレッド単位
    comments: Vec<Comment>,// [0] がルート、以降が返信
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

- `parent_id` 方式より `Thread { comments }` の方が resolve/描画/返信の単位が自明。
- **`pending/submit` は初期スコープから外す**: 永続化も外部投稿も無いため意味が薄い。
  必要なら後で「未確定スレッドをまとめて確定表示する」UI 状態として足す（モデルではなく view flag）。

### 6.1 アンカリング / reload 耐性（hunk も抱える本質課題）
- watch で diff が再読込されると行番号がズレ、コメントが宙に浮く。
- `Anchor` は「行番号」だけに依存しない: `{ hunk_header_hint, context_line_text, offset_in_hunk }` を持ち、
  reload 後に**ベストエフォートで再アンカー**。一致しなければ `orphaned` フラグで「位置不明」表示にして消さない。
- MVP では「reload するとコメントは行番号で再表示、外れたら orphaned 表示」から始め、精度は後で上げる。

### 操作（2入口）
- **TUI 内**: 行/範囲選択 → `c` コメント / `r` 返信 / `R` resolve トグル / `e` 編集 / `d` 削除 / `n`/`N` ジャンプ
- **CLI / agent**: `hew session comment add | reply | resolve | edit | rm | list` + `hew session review --json` で吸い出し

---

## 7. レイアウト / UI

- **split**（左右 old/new）/ **stack**（unified）/ **auto**（幅で自動切替）— hunk 同様。
- サイドバーでファイル間ナビ。
- マウス対応（crossterm のマウスイベント: クリックで行選択、ドラッグで範囲選択、ホイールでスクロール）。
- 折り返し / 行番号 / テーマは config（任意・後回し可）。

---

## 8. Rust スタック

| 領域 | クレート | 備考 |
|---|---|---|
| TUI | `ratatui` + `crossterm` | 描画 + key/mouse |
| diff 生成 | `similar` | 2テキスト/blob から |
| diff 解析 | `diffy` or 自前 | unified patch パース |
| highlight | **`syntect` 先行 → `tree-sitter` 後** | §10 参照 |
| CLI | `clap` (derive) | サブコマンド |
| session server | `tokio` + `axum`（軽量 HTTP/JSON） | loopback |
| async ランタイム | `tokio` | server + watch |
| git | サブプロセス → 後で `gix`/`git2` | §5 |
| watch | `notify` | `--watch` |
| JSON | `serde` + `serde_json` | |
| ID | `uuid` | |

外部連携クレート（`octocrab`/`gh`）は **使わない**。

---

## 9. 性能目標（測定可能にする）

ベンチは hunk の bench 構成を踏襲し、同条件で比較できるようにする。

| 指標 | 目標 | 計測 |
|---|---|---|
| 起動 → 初フレーム（小 diff） | < 50ms | bench: bootstrap |
| 大 diff（10k+ 行）初フレーム | < 200ms | bench: large-stream（ビューポート分のみ） |
| スクロール 1 フレーム | < 8ms（120fps 相当余裕） | bench: render-layout |
| ハイライト | ビューポート分のみ + キャッシュ | bench: highlight |
| メモリ（大 diff） | 入力サイズに対し線形・低係数 | bench: memory |

設計原則:
- 全行を事前にレイアウト/ハイライトしない。**可視範囲 + 先読み少量**のみ計算。
- ハイライト結果・整形済み行は LRU キャッシュ。
- ratatui の差分バッファでフレーム間の再描画を最小化。

---

## 10. ハイライト方針の決定

- **MVP は `syntect`**: 単一クレート + Sublime 文法で導入が速く、言語カバレッジ広い。
- **後で `tree-sitter`**: インクリメンタル解析で大ファイル/編集時に強い。性能仕上げフェーズで、
  ボトルネックになっている言語から差し替え。
- highlight はトレイト（`Highlighter`）で抽象化し、バックエンドを差し替え可能にする。

---

## 11. プロジェクト構成（案）

```
hew/
├─ Cargo.toml
├─ PLAN.md
├─ src/
│  ├─ main.rs            # clap エントリ、サブコマンド分岐
│  ├─ cli.rs             # コマンド定義
│  ├─ diff/
│  │  ├─ model.rs        # DiffFile / Hunk / Line 正規化表現
│  │  ├─ generate.rs     # similar ベース生成
│  │  └─ parse.rs        # unified patch 解析
│  ├─ vcs/
│  │  └─ git.rs          # diff/show（最初はサブプロセス）
│  ├─ comments/
│  │  ├─ model.rs        # Thread / Comment / Anchor
│  │  └─ anchor.rs       # reload 再アンカー
│  ├─ session/
│  │  ├─ server.rs       # axum loopback JSON API
│  │  ├─ protocol.rs     # リクエスト/レスポンス + SessionCommand
│  │  └─ registry.rs     # session 発見（レジストリファイル）
│  ├─ ui/
│  │  ├─ app.rs          # 状態 + レンダループ + イベント/コマンド処理
│  │  ├─ layout.rs       # split/stack/auto
│  │  ├─ diff_pane.rs
│  │  ├─ comment_view.rs
│  │  └─ highlight.rs    # Highlighter トレイト + syntect 実装
│  └─ watch.rs           # notify
└─ tests/                # 統合テスト
```

---

## 12. テスト戦略

- **ユニット**: diff 解析（unified patch の各形）、diff 生成、コメント/スレッドモデル、再アンカー、session protocol の round-trip。
- **ゴールデン/スナップショット**: レイアウト（split/stack）出力を `insta` で固定。
- **統合**: server を立てて `hew session ...` の JSON 契約を検証（コメント追加→review --json に出る等）。
- **ベンチ**: §9 を `criterion` or 自前ハーネスで。CI で回帰検知。

---

## 13. マイルストーン（受け入れ条件つき）

1. **静的 diff 表示** — `hew patch -` / `hew diff <a> <b>` をパース/生成し ratatui で色なし表示・スクロール。
   _完了条件_: 大きめ patch を開いてスクロールが滑らか、クラッシュなし。
2. **git 対応** — `hew diff`（作業ツリー）/ `hew show [rev]`（最初はサブプロセス）。
   _完了条件_: 実リポで diff/show が開ける。
3. **session 基盤** — loopback server + registry + `hew session list / review --json / navigate`。
   _完了条件_: 別プロセスの CLI から稼働中 TUI を navigate できる。
4. **コメント（行/単発）** — `hew session comment add / list` → inline 描画 + TUI 内 `c`。
   _完了条件_: CLI と TUI 両方からコメントが付き、review --json に出る。
5. **PR style コメント** — 範囲 / スレッド(`reply`) / `resolve` / `edit` / `rm` + 折りたたみ UI + `n/N` ジャンプ。
   _完了条件_: スレッドの返信・解決・畳みが TUI/CLI 双方で動く。
6. **性能仕上げ** — syntect ハイライト + ビューポート遅延化 + キャッシュ。§9 目標達成。必要なら tree-sitter / gix へ部分置換。
   _完了条件_: §9 のベンチ目標を満たし、hunk 比で起動/大 diff が明確に速い。

---

## 14. 主要リスク

| リスク | 対策 |
|---|---|
| 同期 TUI と非同期 server の橋渡しが複雑 | mpsc + oneshot に限定、状態は TUI 単一所有（§3） |
| gix の diff/blob が未成熟 | MVP はサブプロセス、後でホットパスのみ置換（§5） |
| reload でコメントが浮く | Anchor で再アンカー + orphaned 表示（§6.1） |
| syntect が大ファイルで遅い | ビューポート分のみ + キャッシュ、最終手段 tree-sitter（§10） |

---

## 15. 未決定事項

- session protocol を **hunk 互換 JSON** にするか独自にするか（互換にすると hunk の skill/agent ワークフローを流用できる）。
- registry をファイル方式のままにするか常駐ブローカへ昇格するか（負荷次第）。
- config フォーマット（toml）とテーマ機構を入れるタイミング。
