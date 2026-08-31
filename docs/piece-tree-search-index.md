# ピースツリーと検索インデックスの連携

## 目的

巨大ファイル（10MB 超）の検索を効率化する。固定インデックスをそのまま保ち、編集による差分をピースツリーと連携させて件数を推定する。

## 前提

- ピースツリーはテキスト列そのものを管理する
- ピースは論理インデックスブロックを介して検索インデックスを参照する
- 検索インデックスは開いた時点の不変な元ファイルに対して構築し、編集で直接書き換えない
- 編集後の実走査単位は論理インデックスブロックに属する子単位として扱う
- 編集差分は `EditEvent` ログとして分離する

## ブロック

### 初期分割と現在サイズ

- 初期構築時は 512KB を目安に、文書のエンコーディングにおける文字の途中で切らないよう分割する
- `BlockId` は初期分割で定まり、後から現在位置に応じて振り直さない
- 編集後のブロックサイズは可変とする
- ブロックの開始バイト位置は保持せず、前方ブロックの現在サイズの累計から求める
- 累計は B ツリーの集約値として保持してよい。ブロックごとの開始位置キャッシュを正しさのための状態にはしない

### 肥大化したブロック

- 現在サイズが大きくなりすぎたブロックだけを実走査単位へ分割する
- 分割後の実走査単位は、分割前と同じ論理インデックスブロックに属する
- 分割しても、後続ブロックの開始位置やサイズを個別に更新しない
- 前後のブロック参照は隣接移動のために持ってよい
- 分割の閾値は実行時のチューニング値とし、ユーザー設定にはしない

### 文字境界

- 初期分割と編集範囲の境界は、文書のエンコーディングにおける文字の途中で切らない
- マルチバイト文字をブロック間で分断しない
- 不正なバイト列はそのまま保持する
- 不正なバイト列を含む範囲は文字として解釈せず、bi-gram 索引の対象外とする

### エンコーディング

- ピースツリーと編集バッファは、文書のエンコーディングのバイト列を扱う
- バイト位置を主座標とし、文字境界の判定と bi-gram の抽出だけでエンコーディングを参照する
- エンコーディングの変換や保存形式の判断は、ピースツリーではなくファイル形式側の責務とする

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

- ピースは元ファイルまたは編集バッファの、文書のエンコーディングによるバイト範囲を参照する
- バイト位置を主座標とし、行番号をピース自身には持たせない
- ピースがブロックをまたぐ場合は、終了側の論理インデックスブロックに所属させる
- ブロック分割が発生しても、隣接する後続ブロックの識別子は変更しない
- 分割後の実走査単位には、分割前の論理インデックスブロックへの参照を持たせる

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

## 編集境界の bi-gram

編集された文字列の内部だけでなく、編集範囲の前後 1 文字を含む境界範囲を差分計算の対象にする。

1 回の編集では、境界範囲について編集前後の bi-gram を比較し、消えたものを負の差分、新しく生じたものを正の差分として記録する。これにより、削除後に前後の文字が接続して生じる bi-gram も扱える。

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
    base_revision: Revision,
    pos: ByteIndex,        // base_revision 時点の文書座標
    delete_len: usize,
    delete_text: BufferRef,
    insert_text: BufferRef,
}
```

- 各操作は、その操作を生成した時点の `base_revision` と、その時点の文書座標を持つ
- 操作を適用すると revision が進む
- 古い revision を基準にした操作を適用する場合は、操作ログで現在座標へ写像してから適用する
- 追記専用
- Undo: `head -= 1`
- Redo: `head += 1`
- 新しい編集: `ops.truncate(head)` して追加、`head += 1`
- Undo/Redo は操作ログの適用範囲を変更し、ピースツリーと検索差分を同じ範囲から再構成する

### 操作ログの生存期間

- ファイル規模によって、保存後の Undo/Redo 保持方針を分ける
- 小さいファイルでは、開いた時点のベースを内部スナップショットとして保持し、保存後も操作ログを保持して Undo/Redo を可能にする
- 巨大ファイルでは内部スナップショットを作らない。保存時に現在の内容を新しいベースとし、操作ログと検索差分を破棄するため、保存後の Undo/Redo はできない
- 巨大ファイルの保存後の検索インデックスは、新しいベースに対して必要に応じて育て直す
- 文書を閉じた時点で、その文書の操作ログと関連する編集実体を破棄してよい
- 文書を開き直す場合は、保存済みファイルまたは下書きから新しい操作ログを開始する
- 小さいファイルと巨大ファイルを分ける閾値は、ユーザー設定ではなく実行時のチューニング値とする

### 巨大ファイルの保存

巨大ファイルでは、保存後の Undo 用スナップショットを作らない。保存は、最も前の編集位置以降だけを対象に行う。

1. 対象ファイル、最初の変更バイト位置、対象 revision、退避先、保存状態を保存ジャーナルへ記録する
2. 最初の変更位置から末尾までの元データを退避する
3. 変更位置より前の prefix は維持し、以降の suffix を現在のピースツリーから再構成して書き込む
4. 書き込み結果と対象 revision を検証する
5. 成功後に保存済みファイルを新しいベースとし、対象ブロックのベース索引へ差分を取り込み、操作ログと差分を破棄する
6. 退避データと保存ジャーナルを削除する

保存途中で失敗またはクラッシュした場合は、退避した suffix から元ファイルを復元し、その元ファイルをベースとして操作ログを再適用する。保存ジャーナルが残っている場合は、起動時に保存途中として復旧する。

複数箇所を一度に編集した場合も、最も前の編集位置から末尾までを1つの保存対象とする。保存成功までは操作ログを破棄しない。

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
| `UNDO_SNAPSHOT_THRESHOLD` | 未定 | 保存後の Undo 用スナップショットを作る上限 |
| `EDIT_BUFFER_INITIAL` | 4_096 (4KB) | 挿入バッファ初期容量 |
| `DELETE_BUFFER_INITIAL` | 4_096 (4KB) | 削除バッファ初期容量 |
| `MEMORY_BUDGET_BYTES` | 33_554_432 (32MB) | 文書ストア全体のメモリ予算 |
| `IDLE_TIMEOUT_MS` | 500 | 入力アイドル判定時間 |
| `LONG_LINE_THRESHOLD` | 1_048_576 (1MB) | 異常に長い 1 行の閾値 |
