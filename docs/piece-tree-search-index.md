# ピースツリーと検索インデックスの連携

## 目的

巨大ファイル（10MB 超）の検索を効率化する。固定インデックスをそのまま保ち、編集による差分をピースツリーと連携させて件数を推定する。

## 前提

- ピースツリーはテキスト列そのものを管理する
- ピースは `block_id` を介して検索インデックスを参照する
- 検索インデックスは開いた時点の不変な元ファイルに対して構築し、編集で直接書き換えない
- 編集差分は `EditEvent` ログとして分離する

## ブロック

### 初期分割と現在サイズ

- 初期構築時は 512KB を目安に、UTF-8 コードポイントの途中で切らないよう分割する
- `BlockId` は初期分割で定まり、後から現在位置に応じて振り直さない
- 編集後のブロックサイズは可変とする
- ブロックの開始バイト位置は保持せず、前方ブロックの現在サイズの累計から求める
- 累計は B ツリーの集約値として保持してよい。ブロックごとの開始位置キャッシュを正しさのための状態にはしない

### 肥大化したブロック

- 現在サイズが大きくなりすぎたブロックだけを論理的に分割する
- 分割しても、後続ブロックの開始位置やサイズを個別に更新しない
- 前後のブロック参照は隣接移動のために持ってよい
- 分割の閾値は実行時のチューニング値とし、ユーザー設定にはしない

### コードポイント境界

- 初期分割と編集範囲の境界は UTF-8 コードポイントの途中で切らない
- マルチバイト文字をブロック間で分断しない

### ブロックまたぎの bi-gram

ブロック `b` の最後の文字 `x` とブロック `b+1` の最初の文字 `y` から成る bi-gram `xy` は、ブロック `b+1` にカウントする。

ブロック `b+1` の先頭には `x` を重複して含める。これにより suffix array 的な扱いが可能。

例：

```
ブロック b    : ... x
ブロック b+1  : x y ...
                 ^ 先頭に前ブロックの最後の文字を重複
```

## ピース

- ピースは元ファイルまたは編集バッファの UTF-8 バイト範囲を参照する
- バイト位置を主座標とし、行番号をピース自身には持たせない
- ピースがブロックをまたぐ場合は、終了側のブロックに所属させる
- ブロック分割が発生しても、隣接する後続ブロックの識別子は変更しない
- インデックスの論理ブロックと、現在の実走査単位を分ける必要がある場合は、同じ論理ブロックに属する走査単位として扱う

ブロックの現在開始位置は、ピースまたはブロックの開始位置として保存せず、順序付き構造の前方サイズ累計から求める。

## 固定インデックス

- 形式: `(bigram, block, count)`
- 不変: 開いた時点の元ファイルを対象に構築
- 編集では書き換えない

## 編集イベントログ

```rust
struct EditEvent {
    edit_id: EditId,
    deltas: HashMap<(BlockId, Bigram), i64>,
}
```

- 粒度: 1 操作 1 イベント
- 値: ネット値（追加は `+count`、削除は `−count`）
- Undo 時は対象の `EditEvent` を log から取り除く

```rust
struct EditLog {
    events: Vec<EditEvent>,
    accumulated: HashMap<(BlockId, Bigram), i64>,
}
```

- `accumulated` は `EditEvent` の差分を即座または idle 時に集計
- 件数推定は `index + accumulated` の合算

## 件数推定

```rust
fn estimated_count(
    index: &Index,
    log: &EditLog,
    bigram: Bigram,
    block: BlockId,
) -> i64 {
    let base = index.count(bigram, block) as i64;
    let delta = log.accumulated.get(&(block, bigram)).copied().unwrap_or(0);
    base + delta
}
```

## 更新タイミング

- `EditEvent` は編集発生時に生成する
- `log.accumulated` の更新はユーザー入力のアイドル時に行う
- 連続入力中は概数を見せ、idle で確定する

## 構造図

```mermaid
flowchart TB
    PT[Piece Tree]
    P[Piece<br/>buffer_id, start, end, block_id]
    EEL[Edit Event Log]
    E[EditEvent<br/>deltas: (block, bigram) -> i64]
    IDX[Fixed Index<br/>(bigram, block, count)]
    EST[Estimated Count]

    PT --> P
    P -->|block_id| IDX
    EEL --> E
    E -->|delta| EST
    IDX -->|count| EST
```

## 責務

- ピースツリー: テキスト列の管理
- 固定インデックス: 開いた時点の不変な検索情報
- 編集イベントログ: 編集による差分の蓄積
- 件数推定: 上記 3 つを合成した推定値

## ピースツリーの編集側

### Piece

```rust
struct Piece {
    source: SourceRef,
    start: ByteIndex,
    end: ByteIndex,
    lines: usize,                  // ピース内行数
    line_breaks: Vec<ByteIndex>,   // 改行文字の直後のバイト位置
    block_id: BlockId,
}

enum SourceRef {
    Disk,
    Edit(EditBufferId),
}
```

- `start_line` は持たず、行番号は B ツリーの `subtree_lines` から計算
- バイト位置が主座標

### BufferRef

`BufferRef` は「テキスト実体を指す参照」です。`Piece` や `Operation` が実際の文字列を持たず、どのバッファのどの範囲かを指します。

```rust
enum BufferId {
    Disk,
    Insert,
    Delete,
}

struct BufferRef {
    buffer: BufferId,
    start: ByteIndex,
    end: ByteIndex,
}
```

- `Disk`: 元ファイル内
- `Insert`: 挿入用 `EditBuffer`
- `Delete`: 削除用 `DeleteBuffer`

### B ツリー

最小度数 4、最大 8 の通常の B ツリー。

```rust
const B: usize = 4;
const MAX_KEYS: usize = 2 * B;   // 8
const MIN_KEYS: usize = B - 1;   // 3

struct BTreeNode {
    is_leaf: bool,
    pieces: Vec<Piece>,
    children: Vec<BTreeNode>,
    subtree_bytes: usize,
    subtree_lines: usize,
}
```

### 操作ログ

```rust
struct OperationLog {
    ops: Vec<Operation>,
    head: usize,  // 現在適用されている先頭
}

struct Operation {
    pos: ByteIndex,
    delete_len: usize,
    delete_text: BufferRef,
    insert_text: BufferRef,
}
```

- 追記専用
- Undo: `head -= 1`
- Redo: `head += 1`
- 新しい編集: `ops.truncate(head)` して追加、`head += 1`

### EditBuffer と DeleteBuffer

- 挿入したテキストは `EditBuffer(Vec<u8>)` へ末尾追記
- 削除したテキストは `DeleteBuffer(Vec<u8>)` へ末尾追記
- `BufferRef` がそれぞれの範囲を指す
- Undo 時は `delete_text` を元の位置へ戻す

### 調整可能なパラメータ

以下は実行時定数として定義し、後から調整可能にする。これらはユーザー設定ではなく、文書ストア内部のチューニング値とする。

| 名前 | デフォルト | 説明 |
| --- | --- | --- |
| `BTREE_MAX_KEYS` | 8 | B ツリー 1 ノードあたりの最大ピース数 |
| `BLOCK_INITIAL_SIZE_BYTES` | 524_288 (512KB) | 初期分割時のブロックサイズの目安 |
| `BLOCK_SPLIT_THRESHOLD_BYTES` | 未定 | 編集後のブロックを分割する閾値 |
| `INDEX_BUILD_THRESHOLD` | 10_485_760 (10MB) | 索引を育て始めるファイルサイズ |
| `EDIT_BUFFER_INITIAL` | 4_096 (4KB) | 挿入バッファ初期容量 |
| `DELETE_BUFFER_INITIAL` | 4_096 (4KB) | 削除バッファ初期容量 |
| `MEMORY_BUDGET_BYTES` | 33_554_432 (32MB) | 文書ストア全体のメモリ予算 |
| `IDLE_TIMEOUT_MS` | 500 | 入力アイドル判定時間 |
| `LONG_LINE_THRESHOLD` | 1_048_576 (1MB) | 異常に長い 1 行の閾値 |
