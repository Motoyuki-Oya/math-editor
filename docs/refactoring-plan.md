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
  + 育てた索引」形式へ置き換える。復元はログ再生で行う。ログは正式保存で削除、
  索引は未利用期間または閾値以下への縮小で削除する。
- seek 基盤と tail 読み(`read_tail`)は維持する。mmap は読み出し器の内部最適化に
  とどめ、前提にしない。
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

### 文書ストアで実装前に確定が必要な事項

- 索引キャッシュのファイル形式、無効判定(サイズ・更新時刻・ハッシュ)、
  「しばらく利用がない」の期間
- バイグラムの正規化(大文字小文字、エンコーディングをまたぐ扱い)と
  ブロック境界の重なり幅
- 操作ログの保持期間(undo 上限と生きているスナップショットの revision の関係)
- 正規表現からのリテラル抽出方式
- ピースツリーのノード構造と編集バッファの持ち方
- 編集実体をメモリに置く上限(メモリ予算)と下書きファイル内の実体の形式
- 置換など導出操作の遅延適用の重ね掛け(置換の上に置換が乗る場合)の扱い
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

- [ ] 最上位と構造内部で入力の規則に差がない(最上位の特権はタブ整列のみ)
- [ ] `/` `^` `_` `(` `[` 矢印がどの深さでも通常文字として入る
- [ ] 構造化は「文字列 + トリガー文字 + スペース」だけで起きる

### Phase 3: 文書ストア(単一の真実 = 操作ログ)

5. 操作ログ(revision)を導入し、`Step` スタックをログ導出へ置き換える。
   `search_snapshot` の結果を写像する。
6. ピースツリー化 + 編集バッファ参照(`Fresh(Vec<String>)` 廃止)。
7. 置換の操作記録 + 遅延適用(結果を実体化しない)。導出できない実体の
   メモリ予算超過分を下書きへ流す。
8. 下書きを「元ファイル参照 + 操作ログ + 育てた索引」形式へ。
9. bigram 差分インデックス(必要に応じて育てる + 永続化)。

チェック項目:

- [ ] ファイル実体をメモリへ乗せる操作・全文を返す API を追加していない
- [ ] seek 基盤と tail 読みが維持されている(mmap を前提にしていない)
- [ ] Undo・下書き・インデックス差分がすべて操作ログから導出されている
      (ログと別の真実を作っていない)
- [ ] 操作が基準 revision を持ち、古い基準の操作はログで現在座標へ変換してから
      適用している(複数ユーザー編集の土台を壊していない)
- [ ] ベース索引は不変な元ファイルだけを対象とし、編集で書き換えていない
- [ ] ビュー/スライス向けの界面がファイルサイズで分岐していない

### Phase 4: MVC(操作ログの消費者)

10. 文書モデル + スライスモデル導入(共有キャッシュ廃止)、revision 通知で更新、
    `sync.rs` のキューを置き換える。読み取り応答の revision 検証を入れる。
11. View の paint 専用化、グローバル状態の所有者移動、`shell.rs` の必要範囲
    分割、単語分割の界面化。
12. 連動グループ + 入力グループ ID の Undo。

チェック項目:

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
