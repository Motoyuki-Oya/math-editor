# ピースツリーと検索インデックスの連携

## 目的

巨大ファイル（10MB 超）の検索を効率化する。固定インデックスをそのまま保ち、編集による差分をピースツリーと連携させて件数を推定する。

## 前提

- ピースツリーはテキスト列そのものを管理する
- ピースは `block_id` を介して検索インデックスを参照する
- 検索インデックスは開いた時点の不変な元ファイルに対して構築し、編集で直接書き換えない
- 編集差分は `EditEvent` ログとして分離する

## ブロック

### サイズ

- 固定 512KB
- ファイル先頭から順に `BlockId` を割り振る

### コードポイント境界の調整

- ブロック境界は UTF-8 コードポイントの終わりに寄せる
- マルチバイト文字の途中を切らない
- ブロックサイズは必ずしも 512KB ぴったりではなく、「512KB 以下」のコードポイント境界になる

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

```rust
struct Piece {
    buffer_id: BufferId,
    start: ByteIndex,
    end: ByteIndex,
    block_id: BlockId,
}
```

- ピースがブロックをまたぐ場合、終了側の `block_id` を採用する
- これによりブロックまたぎの bi-gram を後続ブロックに寄せる方針と整合する

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
