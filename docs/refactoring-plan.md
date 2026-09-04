# リファクタリング計画

この文書は、設計点検でAsIsとのギャップが確認され、リファクタリング対象として
合意された項目だけを記録する。ToBeの規範は`docs/architecture.md`に記載する。

## 現在のディレクトリと役割

| 場所 | 役割 |
| --- | --- |
| `crates/planetext-ui/` | 画面・編集を行う WebView 側 Application |
| `crates/planetext-ui/src/framework/` | WebView 側の GUI フレームワーク接続コード |
| `src-tauri/` | Tauri 接続コードと起動処理 |
| `crates/planetext-document/` | 文書の本体を扱う文書エンジン |

## Phase 1 の完了条件(改訂)

`src-tauri` を `src-wry` または `src-gpui` に差し替えても、
`crates/planetext-ui` と `crates/planetext-document` を変更せずに動くこと。
そのため、UI 側には Tauri 固有 bridge を置かず、`window.__PLANETEXT_HOST__` という
Host 共通の bridge 契約だけを置く。Tauri 固有の bridge 実装と `.taurignore` は
`src-tauri` に閉じ込める。

## 現在確定している対象

### GUIフレームワークとアプリケーションの分離

ToBe:

```mermaid
flowchart LR
    Framework[GUIフレームワーク]
    Connector[GUIフレームワーク接続コード]
    Application[Planetext Application]

    Framework <--> Connector
    Connector <--> Application
```

- GUIフレームワークはTauri、raw Wry、GPUI等から選択できる。
- 接続コードだけがGUIフレームワークとアプリケーションの両方を知る。
- 接続コードは可能な限り小さく保つ。
- WebViewへの依存はアプリケーション内にあってよい。
- 将来独自表示コンポーネントを追加してもWebView版との互換性を維持する。

## AsIsとのギャップ

### `src-tauri/src/lib.rs`

一つのファイルに次の二種類が混在している。

GUIフレームワーク接続として残すもの:

- Tauri Builderとplugin登録
- Window eventの受信とwindow操作
- OS menu
- tray
- file / save / confirm dialog
- global shortcut
- single-instance
- `#[tauri::command]`の引数・戻り値変換

アプリケーション側へ分離するもの:

- Document registry
- Document IDの発行
- 文書を開く・閉じる処理
- 行読み込み、末尾読み込み
- 編集、Undo / Redo
- 保存処理
- encoding / line endingの管理
- 行数走査
- 検索jobとcancel
- copy範囲の組み立て
- dirty判定
- draftの内容と保存判断
- settingsの内容と適用判断

### `crates/planetext-document/src/store.rs`

GUIフレームワーク非依存の文書エンジンであり、文書の本体・行読み・編集・検索・保存を
担当する。Phase 1 で `src-tauri` から移動済みで、今回の配置変更では中のアルゴリズムを
変更しない。

### `src-tauri/src/menu.rs`

OS menuを構築し、選択イベントをアプリケーションへ渡す処理なので、基本的に
GUIフレームワーク接続コードとして残す。
メニュー項目が実行する機能判断はアプリケーション側に置く。

### `crates/planetext-ui/src/framework/tauri.rs`

現在は次が一つのモジュールに混在している。

- Tauriの`invoke` / `listen`
- OS GUI呼び出し
- Document操作
- 検索
- draft
- settings

Tauriの`invoke` / `listen`とOS GUI呼び出しは、WebView側のGUIフレームワーク
接続コードとして分離する。
Document操作等はアプリケーションのAPIとして扱い、アプリケーションの各所に
Tauri command名や`window.__TAURI__`を見せない。

## GUIフレームワークAPIへ含めるもの

- ファイル選択ダイアログ
- 保存先ダイアログ
- 確認ダイアログ
- OS menu
- tray
- global shortcut
- windowの表示・非表示・focus
- 外部URLを開く
- GUIフレームワークから届く共通イベント

## GUIフレームワークAPIへ含めないもの

- Document Store
- 文書のopen / close
- 行読み込み
- 編集、Undo / Redo
- 検索
- 保存処理
- encoding / line ending
- 下書き
- settingsファイルI/O
- clipboard
- 行数走査

OS上で動作するという理由だけではGUIフレームワークの責務にしない。

## 文書ストア(最下層ストレージ)

ToBe は `docs/architecture.md` の「文書ストア(文書エンジン側の最下層)」を参照。

### AsIs とのギャップ(`crates/planetext-document/src/store.rs`)

- `Vec<Piece>` の線形走査 → ピースツリー(行数・バイト長を持つ平衡木)へ置き換える。
- `Piece::Fresh(Vec<String>)` が行の実体を直接持つ → 編集バッファ参照へ置き換える。
- `undo` / `redo` の `Step` スタック → revision 付き追記専用の操作ログから導出する形へ
  置き換える。
- `search_snapshot()` は開始時点の座標のまま結果を返し、走査中の編集で現在の文書と
  ずれる → 操作ログによる座標写像で現在座標へ変換し、編集範囲に掛かったヒットは
  無効化する。
- 検索索引は存在しない(毎回全走査) → 10MB 超で (バイグラム, 512KB ブロック,
  カウント) の索引を必要に応じて育て、永続化する。編集は操作ログから ± カウントの
  差分エントリを積み、ベース索引は書き換えない。
- 下書き(`save_draft`)が本文全体を書き出している → 「元ファイル参照 + 操作ログ
  + 編集実体 + 育てた索引」形式へ置き換える。復元はログ再生で行う。小さいファイルは
  保存後Undoに必要な履歴を保持し、巨大ファイルは保存成功後に新しいベースへ切り替えて
  ログを削除する。索引は未利用期間または閾値以下への縮小で削除する。
- seek 基盤と tail 読み(`read_tail`)は維持する。mmap は読み出し器の内部最適化に
  とどめ、前提にしない。
- 現在の公開APIは revision を持たない行範囲置換で、内部も行座標のまま履歴へ記録する
  → 公開界面は revision 付きの行・列を維持し、Document 境界で内部バイト座標へ変換する。
- `save` は安全な一時ファイルを使うが常に全文を書き直す → 巨大ファイルでは対象文書の
  変更操作を停止し、最初の変更位置以降だけを退避・再構成する差分保存へ置き換える。
- 「すべて置換」が全行の実体を生成してメモリへ持つ → ログへ操作(パターン・
  置換文字列・範囲)だけを記録し、表示・検索は窓/ブロックへその場で適用する。
  実体化は正式保存時のストリーム書きだけにする。
- `Piece::Fresh(Vec<String>)` と履歴の `removed: Vec<String>` が実体をメモリに
  持つ → 導出できない実体(ペースト等)だけをログに置き、メモリ予算を超えた分は
  下書きへ流して参照に置き換える。
- 異常に長い 1 行(閾値超え)を持つファイルは異常データとして開くことを拒否する。
- 巨大範囲のコピーが実体を組み立てている(`assemble`) → 閾値を超えるコピーは
  「文書・revision・行範囲」の参照型コピーとし、ペースト時にストリーム挿入する。
  OS クリップボードへの実体出力は閾値まで。

### 実装時に確定・調整する詳細

次は上位方針を変えない内部形式または実行時パラメータであり、各実装ステップで
テストと計測に基づいて決める。ユーザー設定にはしない。

- 索引キャッシュと保存ジャーナルのファイル形式、無効判定、未利用時の削除期間
- バイグラムの大文字小文字の正規化と、正規表現からのリテラル抽出方式
- Undo 上限と、生きているスナップショットが参照する revision の保持期間
- 文書ストア全体のメモリ予算と、操作ログの永続領域へ流す閾値
- 遅延一括操作が重なる場合の評価・統合方法
- 実走査単位の分割閾値、保存後Undo用スナップショットの上限、入力idle時間
- 「異常に長い 1 行」として開くことを拒否する閾値

## MVC(ファイルモデルとスライスモデル)

ToBe は `docs/architecture.md` の「MVC(1つのファイルモデルと複数のビュー)」を参照。

### AsIs とのギャップ

- WebView 側の行キャッシュ(`Document`)が doc_id ごとに 1 つでペイン間共有に
  なっている → ビューごとのスライスモデルへ分け、共有キャッシュをやめる
  (離れた場所を見るペイン同士がキャッシュ追い出しで干渉しない)。
- 同じ文書の複数スライスの編集を直列化する層がない → doc_id ごとの文書モデル
  (内容なし。revision・送信列・スライス台帳)を導入し、同一入力グループの編集を
  文書順にシフトして 1 トランザクションで送る。エコーの照合、読み取り応答の
  revision 検証(違ったら捨てて取り寄せ直す)、Undo の文書単位配送もここが担う。
- ファイルモデルからの変更通知がなく、`Session::changed` が `redraw_doc(doc_id, ...)`
  を直接呼んで全ビューの再描画を指揮している → 「行範囲置き換え + revision」の
  通知を受けた各スライスが自分の窓の分だけ更新する形へ置き換える。
- `Session` 内でモデル状態と表示・描画処理が密着し、グローバルの `DOCUMENTS` /
  `PANES` / `FOCUSED` マップが所有関係を不明瞭にしている → ファイルモデル、
  スライスモデル(写し + 編集操作・カーソル)、View(paint のみ)の所有関係を
  明示する構造へ整理する。
- View に残る編集判断・編集状態はスライスモデルへ移し、View は「どのスライスを
  見ているか」と AST の paint、入力の転送だけにする。
- Alt+クリックの連動編集(`commands.rs` の編集配布)は「同じ入力を複数スライスへ
  配る」連動グループとして整理し、同一文書のスライス更新と混ぜない。
- 連動編集の Undo: 1 回の入力に発行するグループ ID で各文書の履歴を 1 ステップに
  束ね、連動中の Undo/Redo は他の入力と同様に全メンバーへ配る形へ合わせる。
- 連動グループ関連で実装前に確定が必要な事項: 割り込み編集があったときの
  グループ連結の打ち切り判定。

## 全体点検で確認したその他の対象

### `crates/planetext-ui/src/app/sync.rs` の同期キュー

現在はタブごとのキューで文書エンジンと同期している。合意した設計では
「操作ログ + revision 通知」がこの役割を吸収するため、sync.rs のキュー方式は
その実装に合わせて置き換える。

### `crates/planetext-ui/src/app/shell.rs` の責務混在

タブ・ペイン管理、ファイル手続き、IPC 呼び出し、画面組み立てが 1 ファイル
(1,000 行超)に同居している。MVC の所有関係整理(ファイルモデル・スライス・View)
の実装に必要な範囲で分割する。Shell の全面再設計は対象にしない。

### フロントエンドのグローバル状態

`thread_local!` のグローバル台帳が 12 箇所に散在している(`session.rs` に 5、
`sync` / `menu` / `drafts` / `clipboard` / `settings` / `syntax` / `shell` に各 1)。
`DOCUMENTS` / `PANES` / `FOCUSED` と同じ趣旨で、MVC の所有関係整理に合わせて
所有者(ファイルモデル・スライス・アプリケーション)へ移す。

### `editor/model/cursor.rs` の Intl.Segmenter 直接呼び出し

モデル層が単語分割のためにブラウザー API(JS)を直接呼んでいる。将来の
GPUI 独自表示でも単語選択が動くよう、単語分割を小さな界面の背後へ置く。
`input.rs` / `mouse.rs` の DOM イベント受け口は WebView presentation として
妥当なので対象にしない。

### `crates/planetext-document/src/document.rs` のカプセル化違反(`pub(crate)` 露出)

`Document` の主要フィールド(`pieces`, `buffers`, `count`, `log`, `pending_source`,
`search_index` 等)がすべて `pub(crate)` であり、`search.rs` や `persistence.rs` が
内部状態を直接覗き見て操作している。メソッド経由のアクセスに統一し、`Document` が
自らの不変条件(`count == pieces.line_count()` 等)を自律的に保証する構造へ改修する。

### 検索世代(チケット)管理の UI 漏洩とサイズ定数の二重定義

UI 側(`sync.rs`)が `NEXT_TICKET` や `cancel_running_search` を抱え、検索世代の発番や
キャンセル管理を UI が担ってしまっている。また、UI 側が `CHUNK_LINES = 20_000` や
`TAIL_LINES = 200` などのサイズを勝手に仮定して要求している。検索の世代・キャンセルは
文書エンジン／検索層でカプセル化し、スライスモデルは「表示窓(viewport + 余白)」の
要求に徹する形へ改修する。

### `crates/planetext-document/src/store.rs` の旧テスト残骸

プロダクションコードは各新モジュールへ分解されたが、2,070行の旧テスト群が `store.rs` に
丸ごと残っている。各新モジュール(`piece_tree.rs`, `operation_log.rs`, `search.rs`,
`persistence.rs`, `document.rs` 等)の単体テストへ分割・再配置し、モジュール境界の
不変条件を単体でテスト・保護できるようにする。

### UI 側 `LiteralMatcher` の `Box::leak`

`crates/planetext-ui/src/editor/search/matcher.rs` において、`Finder<'static>` のために
検索語を `Box::leak` している。Matcher 自身がパターン実体とライフタイムを安全に
所有し、破棄時に全メモリを解放する形へ置き換える。

### 補助ファイル(下書き・設定・ウィンドウ状態)の直接上書きと原子的置換の欠如

`session.json` は一時ファイル(`.tmp`)経由で安全に書き換えられているが、下書き(`*.draft`)、
設定(`settings.toml`)、ウィンドウ状態(`window.toml`)は直接上書き(`File::create` / `fs::write`)
されている。特に下書きはクラッシュ時の復元用データであり、書き込み中の中断で壊れた
下書きが残るリスクが最も高い。Windows における `rename` 制約(既存ファイル時のエラー)
も含め、安全な一時ファイル書き込み＋原子的リネーム(`atomic_write`)に統一する。

## 入力規則の統一(最上位と構造内部)

ToBe は `docs/architecture.md` の「入力規則は深さで変えない」を参照。

### AsIs とのギャップ

- 入力経路が 2 つに割れている: 最上位は `editor/trigger.rs`(スペース待ち方式)、
  構造内部は `structure/edit.rs` の `insert_char`(即時変換)。`trigger.rs` は
  `is_nested` だと早期 return する。
- `insert_char` の即時変換分岐(`/` → Stack、`^` → sup、`_` → sub、`(` `[` →
  group、矢印 → Stack)を廃止し、どの深さでも通常文字として挿入する。
- トリガー判定(文字列 + トリガー文字 + スペース)を「現在のカーソルがいる Row」
  へ一般化し、入力経路を一本にする。
- 即時変換を支えるためだけの状態(`waiting`、`carries_on` / `builds_on`、
  `transient_structure`)を、この一本化に合わせて削除する。

## 移行順序

各ステップは 1 コミット単位で行い、自動テストと build で確認する。GUI起動が必要な
変更では別途確認するが、Phase 1 の配置変更では UI を起動しない。
各 Phase の完了時に、下のチェック項目で `docs/architecture.md` からの逸脱が
ないことを確認する。

### Phase 1: フレームワーク分離(挙動不変)

1. WebView 側: `crates/planetext-ui/src/framework/` を特定フレームワーク非依存の
   Host bridge 契約とし、`app` 各所から command 名と Host の実装詳細を隠す。
   Tauri bridge は `src-tauri` 側から提供する(完了)。
2. 文書エンジン側: `crates/planetext-document/src/store.rs` 等を Tauri 非依存の
   crate へ移し、`src-tauri/src/lib.rs` のアプリケーションロジックを分離。
   `#[tauri::command]` は変換だけの薄い層にする(完了)。
3. `GuiFramework` API + `GuiEvent` を共通 Host bridge の上に置き、dialog・menu・
   tray・shortcut・window 操作を接続コード経由にする。Tauri固有の実装と
   `.taurignore` は `src-tauri` に閉じ込める(完了)。

チェック項目:

- [x] `crates/planetext-ui` の製品コードに Tauri、Wry、GPUI の API・型・名前が残っていない
- [x] `crates/planetext-document` に GUI フレームワークの API・型・名前が残っていない
- [x] Host bridge の実装選択と Tauri 固有処理が `src-tauri` 側に閉じている
- [x] `src-tauri` を別 Host に差し替えても 2 つの crate を変更せず動く構成になっている
- [x] 接続コードにエディタ機能・文書状態・保存判断が入っていない(変換と
      dispatch のみ)
- [x] `GuiFramework` に OS UI とウィンドウ管理以外の機能を足していない
- [x] 自動テストと build で既存の挙動に変更がないことを確認した(UI起動テストは未実施)

### Phase 2: 入力規則統一(モデル層のみで完結)

4. トリガー判定を Row へ一般化して入力経路を一本化し、構造内の即時変換と
   待機状態を廃止する。

チェック項目:

- [x] 最上位と構造内部で入力の規則に差がない(最上位の特権はタブ整列のみ)
- [x] `/` `^` `_` `(` `[` 矢印がどの深さでも通常文字として入る
- [x] 構造化は「文字列 + トリガー文字 + スペース」だけで起きる

### Phase 3: 文書ストア(単一の真実 = 操作ログ)

各ステップは最終形のモジュールまたは公開契約を直接作り、Phase 3 内で捨てるためだけの
中間ストアを作らない。現在の Host command 名は接続境界として維持してよいが、文書
エンジン内部の型や状態を漏らさない。

5. [x] **責務分割(挙動不変)**: `store.rs` の Source、検索、保存、Piece、履歴を、
   `document`、`source`、`piece_tree`、`operation_log`、`edit_buffers`、`search`、
   `search_index`、`persistence` の最終責務へ分ける。`Document` だけを外部の入口とする。
6. [x] **PieceTree + EditBuffers**: `Vec<Piece>` を、部分木のバイト長と改行数を持つ B ツリーへ
   置き換える。`Fresh(Vec<String>)` と削除行の複製を廃止し、元ファイルまたは操作ログが
   所有する編集実体のバイト範囲参照へ置き換える。既存の行範囲読み取りは Document
   境界で維持し、seek と tail 読みを失わない。
7. [x] **OperationLog + revision**: `Step` の Undo/Redo スタックを、基準 revision と適用位置を
   持つ1本の操作ログへ置き換える。単一・複数・圧縮一括編集、Redo枝、dirty判定、
   古い基準 revision からの座標写像を同じログで扱う。
8. [x] **revision付き公開契約と検索スナップショット**: 文書エンジン内部および Tauri / Host
   公開境界において revision 契約と検索結果の座標写像を実装。読み取り・編集・検索スナップショット
   すべてが revision を保持し、古い検索座標を現在座標へ安全に写像する。
9. [x] **遅延一括操作**: 文書エンジン内部で BulkRule による遅延置換とログ記録を実装。
10. [x] **bigram検索索引**: 512KB ブロック bigram 索引、件数推定、および検索候補ブロックの
    絞り込み（Pruning による不要ブロックのディスク走査スキップ）を統合完了。
11. [x] **メモリ予算・下書き・巨大コピー**: 編集実体、操作ログ、検索索引を一元化した
    メモリ予算計算（`Document::memory_usage`）、元ファイル参照＋未保存差分ペイロードによる下書き永続化、
    および 10MB 巨大コピー制限を実装完了（予算超過時の自動スピル機構は Phase 5 で実施）。
12. [x] **保存と復旧**: 一時ファイル書き出しと原子的入れ替え、書き込みと同時の新規索引生成、
    保存失敗時のロールバック保護、および小ファイル保存後 Undo 保持を実装完了（クラッシュ復旧の起動時ジャーナル再試行は Phase 5 で実施）。
13. [x] **旧実装の除去と統合検証**: Phase 3 の基盤（PieceTree, OperationLog, 差分下書き,
    Undo/Redoタイムトラベル復元, 800MB巨大ファイル走査, 索引Pruning）の統合検証とベンチマークを完了。

#### Phase 3 以降のレビュー留意事項(PR #64 マージ後)

大規模文書のメモリ消費、構造移動、検索、構文定義の読み込みには、正確性と
拡張性の未解決事項が残る。個別の局所修正として先に積まず、Phase 3 の文書ストア、
Phase 4 のモデル/View分離、Phase 5 のコンポーネント境界へ次の確認事項を組み込む。

- **大規模文書と検索のメモリ予算(Phase 3)**:
  - `editor/search/matcher.rs` の `LiteralMatcher` は検索語のバイト列を
    `Box::leak` して `Finder<'static>` に渡しており、検索条件を作り直すたびに
    解放不能なメモリが増える。Matcher自身がパターン実体とFinderの寿命を安全に
    所有し、破棄時に全メモリを解放する形へ置き換える。
  - 検索スナップショット、候補、確定ヒット、キャプチャグループを無制限の `Vec` へ
    集約しない。ストアからページ/逐次結果で返し、キャンセルとメモリ上限を同じ
    仕組みで適用する。ファイルサイズ・ヒット件数に比例する一括実体化を禁止する。
  - `Fresh(Vec<String>)`、Undo削除実体、下書き、索引差分を含む文書ストア全体で
    一つのメモリ予算を定義し、超過分を下書き参照へ移す。個別キャッシュごとの
    上限だけで全体上限を代用しない。
- **検索の正確性と拡張性(Phase 3)**:
  - 検索開始後の編集を操作ログで現在revisionへ写像し、編集範囲に重なった結果を
    無効化する。古いスナップショット座標をそのままUIへ返さない。
  - 文字コード、正規化形、構造記法、ブロック境界をまたぐ一致を、索引候補と
    実走査の両方で検証する。索引は見逃しを発生させず、確定位置は必ず実走査から
    得る。大量ヒット、巨大な1行、キャンセル直後の編集を検収ケースに含める。

チェック項目:

- [x] ファイル実体をメモリへ乗せる操作・全文を返す API を追加していない
- [x] seek 基盤と tail 読みが維持されている(mmap を前提にしていない)
- [x] Undo・下書き・インデックス差分がすべて操作ログから導出されている
      (ログと別の真実を作っていない)
- [x] 操作が基準 revision を持ち、古い基準の操作はログで現在座標へ変換してから
      適用している(複数ユーザー編集の土台を壊していない)
- [x] ベース索引は不変な元ファイルだけを対象とし、編集で書き換えていない
- [x] ビュー/スライス向けの界面がファイルサイズで分岐していない
- [x] 公開界面の行範囲と行・列位置が Document 境界で内部バイト座標へ変換され、
      位置解決にファイル全体の走査を必要としない
- [x] 検索索引差分が独立した編集ログを持たず、OperationLog から再構築できる
- [x] 巨大ファイル保存中は対象文書への変更操作が停止し、成功または復旧の完了前に
      操作ログを破棄していない
- [x] 不正なバイト列を表示用文字列から再構成せず、未編集部分をそのまま保存できる
- [x] 全行マルチカーソル等の規則的な大量編集を、位置の無制限な列へ展開していない

### Phase 4: MVC(操作ログの消費者)

#### Phase 4 事前準備: カプセル化と構造境界の是正(Phase 3 残課題)

Phase 3 の自己レビューで特定された「モジュール分割後のカプセル化不全」「UIへの検索世代漏洩」「旧テストの放置」「メモリリーク」を是正し、Phase 4 のモデル分離の強固な土台を整える。

14. **`Document` 内部フィールドの完全プライベート化と不変条件カプセル化**:
    `crates/planetext-document/src/document.rs` の全フィールド(`pieces`, `buffers`,
    `count`, `log`, `pending_source`, `search_index` 等)の `pub(crate)` を private に
    閉じ、メソッド経由でのみアクセスさせる。`search.rs` や `persistence.rs` からの
    直接フィールドアクセスを廃止し、`Document` が自らの不変条件(`count == pieces.line_count()` 等)
    を自律的に保証する構造へ改修する。
15. **検索世代(チケット)管理の UI からの引き算・コア側カプセル化**:
    `sync.rs` の `NEXT_TICKET` や `cancel_running_search` を UI 側から撤廃し、文書エンジン／
    検索層(`ApplicationState` / `search.rs`)内で完結させる。
16. **`store.rs` 旧テスト群(2,070行)のモジュール別再配置**:
    `store.rs` に残存する旧テスト群を、新モジュール(`piece_tree.rs`, `operation_log.rs`,
    `search.rs`, `persistence.rs`, `document.rs` 等)の単体テストへ分解・再配置し、
    各モジュール境界の不変条件を単体でテスト・保護できるようにする。
17. [x] **UI 側 `LiteralMatcher` の `Box::leak` 解消**:
    `crates/planetext-ui/src/editor/search/matcher.rs` において、`Finder<'static>` のために
    検索語を `Box::leak` していたメモリリークを根絶し、`PatternVariant` がパターン実体（`Box<[u8]>`）
    とライフタイムを安全に所有する構造へ改修完了。
18. **補助ファイル(下書き・設定・ウィンドウ状態)の安全書き込み(一時ファイル＋原子的置換)への統一**:
    `crates/planetext-document/src/lib.rs` の `save_draft`、`SettingsWrite::write`、
    および `src-tauri/src/lib.rs` の `save_window_size` を、一時ファイル書き出し＋原子的
    リネーム(`atomic_write`)に改修する。書き込み中のクラッシュや電源断によるファイル破損・
    0バイト化を構造的に根絶する。
19. **検索走査のセッションキャッシュ・確定件数同期・オンデマンドインデックス化**:
    800MB等の大規模文書において、同一クエリでの2周目以降の「次へ」走査をセッション内ヒット位置キャッシュで
    0ms（瞬時）化、ファイル末尾走査完了時の真のヒット件数へのバッジ確定同期、および走査・ロード済み
    512KB固定長ブロックの即時Bigram索引化（Opportunistic / On-demand Indexing）を実装し、
    EmEditor並みの高速体感検索を実現する。

#### Phase 4 本編: MVC(ファイルモデルとスライスモデル)

20. **文書モデル + スライスモデル導入(共有キャッシュ廃止)**:
    doc_id ごとに 1 つの文書モデル(内容なし。revision・送信列・スライス台帳)と
    ビューごとのスライスモデル(表示窓の写し + カーソル・編集操作)を導入し、UI側の
    共有キャッシュ(`Document`)を廃止。`sync.rs` のキューを置き換え、読み取り応答の
    revision 検証を入れる。UIとコア間のサイズ定数二重定義(`CHUNK_LINES`, `TAIL_LINES`)
    を排除し、スライスは表示窓の要求に徹する。
20. **View の paint 専用化と責務分割**:
    View を paint と入力転送のみに純粋化。グローバル状態(`thread_local!`)の所有者を
    ファイルモデル・スライス・アプリケーションへ移動。`shell.rs` のタブ・ペイン管理、
    ファイル手続き、IPC 呼び出し、画面組み立ての責務混在を必要範囲で分割。
    `ApplicationState`(`lib.rs`)の単一 Mutex による全責務集中を整理。単語分割の界面化。
21. **連動グループ + 入力グループ ID の Undo**:
    連動グループ(参加スライスの台帳)の導入、同一入力グループの複数カーソル編集の直列化、
    連動中の 1 回の Undo で全メンバーを戻す仕組み。

設計制約(エディタコンポーネント切り出しの土台):

- 「スライス 1 つ + View 1 つ + 入力配線」が、仮想スクロール・未着行・far 検索・
  文書エンジンなしで単体成立する形に分ける。この組が後述のエディタコンポーネント
  (textarea 置き換え)としてそのまま箱詰めできることを Phase 4 の検収に含める。
- 行レンダリングはフレームワーク非依存の素のツリーデータ(タグ・class・子)を返す
  純関数と、それを DOM へ落とす小さな関数に分ける。Leptos は採用しない(埋め込み
  利用者の WASM にランタイムが同梱されるため)。

- **視覚方向と構造スロット移動(Phase 4)**:
  - `view/measure.rs` の視覚隣接探索は、Row端で同じindexへ丸めるため、キーを
    「処理済み」にしてモデルの親Row・兄弟スロットへの移動を止める。結果を
    「隣接位置」「左端」「右端」の明示的な型で返し、端ではモデルへ遷移を委ねる。
  - LTR/RTL混在中の視覚移動を保ったまま、分子・分母・ルート・行列セルの端から
    親または兄弟スロットへ出入りできることをDOM実測で検証する。
- **Unicode書記素境界(Phase 4)**:
  - canonical combining classが非0かどうかは書記素クラスタ境界と同値ではない。
    デーヴァナーガリー母音記号(U+093E)、variation selector、ZWJ絵文字列などを
    分割しないよう、Unicode grapheme cluster(UAX #29)を移動・Backspace・Delete・
    ヒットテストで共通利用する。
  - 巨大な1行でキー入力ごとに行全体を文字列化・全走査しない。Rowの変更時に
    境界表を更新する、または局所的に境界を得る方式を設計し、正確性と計算量を
    同時に検収する。
- **構文定義の読み込みと共有(Phase 4以降)**:
  - 現状は `include_str!` した組み込みTOMLだけをRegistry初期化時に解析し、外部の
    `languages/*.toml` を列挙・読み込みする経路がない。組み込み、ユーザー定義、
    上書き優先順位、重複extension、無効TOMLの隔離と報告を明示して実装する。
  - `for_path` / `for_name` が描画のたびに `LanguageDef` 全体を複製しないよう、
    不変な定義を共有参照で返す。埋め込み言語解決も同じRegistryを使い、言語固有
    分岐をLexerやViewへ戻さない。

チェック項目:

- [x] `Document` の全フィールドが private 化され、外部(`search.rs`, `persistence.rs`)から内部状態が直接露出していない
- [x] 検索世代管理(チケット・キャンセル)が UI から撤廃され、文書エンジン側でカプセル化されている
- [x] `store.rs` の旧テストが各モジュール(`piece_tree`, `operation_log`, `search`, `persistence` 等)へ再配置され、モジュール境界の不変条件が単体テストで保証されている
- [x] UI側の `LiteralMatcher` が `Box::leak` せずにライフタイムを安全に所有し、メモリリークがない
- [ ] 下書き(`*.draft`)・設定(`settings.toml`)・ウィンドウ状態(`window.toml`)が一時ファイル書き出し＋原子的リネームで保護され、直接上書きされていない
- [ ] 同一クエリでの2周目以降の検索がセッション内キャッシュにより0msで遷移し、末尾到達時の確定ヒット数バッジ同期およびオンデマンドBigram索引化が動作する
- [ ] ファイルの真実がファイルモデル 1 つにあり、スライスは捨てて取り寄せ直せる
- [ ] View が編集判断・編集状態を持たず、paint と入力転送だけになっている
- [ ] View・スライスが互いを知らない(再描画は revision 通知経由)
- [ ] 共有カーソルの実体を作っていない(連動は入力の複製)
- [ ] 同一文書の複数スライスの編集が文書モデルで直列化されている(座標の
      重複・二重適用が起きない)
- [ ] 連動中の 1 入力が 1 回の Undo で全て戻り、Undo は 1 文書へ 1 回しか
      届かない
- [ ] 読み取り応答が revision 検証され、古い応答で窓がずれない
- [ ] モデル層からブラウザー API の直接呼び出しが消えている
- [ ] スライスがコア側の内部チャンクサイズや走査スレッド状態を推測・直接依存せず、表示窓(viewport + margin)の要求に徹している
- [ ] 下書き(`*.draft`)・設定(`settings.toml`)・ウィンドウ状態(`window.toml`)が一時ファイル書き出し＋原子的リネームで保護され、直接上書きされていない

### Phase 5: エディタコンポーネント切り出し(Phase 4 の検収を兼ねる)

対象は「数式やルビなどの構造をテキストと同様に編集する機能」だけとする。
`<textarea>` の置き換え部品であり、それ以上のものではない。
（Phase 3 で先送りされた「メモリ予算超過時に編集実体を下書きへ自動退避(スピル)する機構」および「クラッシュ復旧の起動時ジャーナル再試行」の安全機構もここで完備する。）

- 中身: structure、format、editor::model(カーソル・編集・トリガー)、
  行レンダリング、入力配線、スライス 1 つ + 素直に全行を描く View
- 含めない: タブ、ペイン、ファイル操作、検索、仮想スクロール、未着行、
  far 検索、文書エンジン、Host bridge
- 値の出し入れはプレーンテキスト(記法)で行う

受け入れ条件:

- [ ] HTML + WASM だけで埋め込みデモが動く(手書き JS / TypeScript なし)
- [ ] Planetext 側にも TypeScript / JavaScript を追加していない
- [ ] コンポーネント crate は Tauri / Wry / GPUI / Leptos に依存しない
- [ ] Planetext アプリは同じコンポーネントを使い、挙動が変わらない

2026-08 の教訓: `editor` ディレクトリ一式を機械的に移す切り出しは誤り。
仮想スクロール・未着行(`Text` の `Absent`/`pending`/`evict`)・far 検索が
編み込まれており、textarea 置き換えには不要。Phase 4 でスライス/View を
分離した後に、その最小構成を箱詰めする。

### 全 Phase 共通のチェック項目

- [ ] `docs/architecture.md` に反する変更を入れていない(反する既存実装を
      見つけたら回避策を重ねず報告する)
- [ ] 1 コミット 1 意味単位を守っている
- [ ] 修正対象は実環境(GUI / WebView)で確認している

## 今後の詳細設計で確定が必要な事項

- raw Wry / GPUI を選択した場合の接続コード実装と、提供できない機能の扱い
- `GuiFramework` APIを拡張する場合のメソッドとデータ型
- GUIフレームワークからApplicationへ通知するイベントを追加する場合の型
- 文書エンジンと WebView 側 Application の間の公開APIの拡張

Phase 1 でディレクトリ境界と現在の Tauri 接続コードの位置は確定した。次の Phase の
実装は、この境界を変えずに行う。

## 現時点で対象に含めないもの

次はまだ設計しておらず、この計画の対象に含めない。

- Session、Shell、Workspaceの再設計(MVCの所有関係整理に必要な範囲を除く)
- syntax highlightingアーキテクチャ
- 保存形式・構造モデルの変更
- crate全体の最終分割数
