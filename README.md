# MathNote

数式を「記法を意識せず」書けるテキストエディタです。分数やルートはキー操作／パレットで挿入でき、
LaTeX や MathML を手で書く必要はありません。本文はプレーンテキストで、構文ハイライトや装飾機能は
持たない「数式が書けるメモ帳」を目指しています。

- Tauri v2 + Leptos（Rust のみ、TypeScript なし）
- 数式エンジンは自作（外部の数式ライブラリ非依存、AST → LaTeX / MathML）
- 保存形式は Markdown 互換のプレーンテキスト（インライン `$...$` / 独立行 `$$...$$`）

## Windows 版のダウンロード

GitHub Actions の `build` ワークフローが `windows-latest` でインストーラを生成します。

- 各コミットのビルド: Actions → 対象の実行 → Artifacts の `mathnote-windows`（`.msi` と `.exe`）
- リリース: `v` から始まるタグ（例 `v0.1.0`）を push すると、同じ成果物が Release に添付されます

## 数式の書き方

本文を打っていると数式に切り替わります。メニューやボタンを使う必要はありません。

| 本文での入力 | 結果 |
| --- | --- |
| `1/` `x/` | 直前の英数字を分子にした分数に入る（`and/or` のような語はそのまま） |
| `x^` `x_` | 上付き / 下付き |
| `\sqrt` + スペース | ルート（`\sum` `\int` `\alpha` `\cases` なども同様） |
| `$` | 空の数式に入る |
| `Esc` | 本文に戻る |

`Ctrl+M`（インライン）/ `Ctrl+Shift+M`（独立行）とパレットも従来どおり使えます。

| 数式内の操作 | 入力 |
| --- | --- |
| 分数 | `/`（直前の英数字が分子になる） |
| 上付き / 下付き | `^` / `_` |
| ルート・総和・積分・行列・ギリシャ文字 | `\sqrt` などのコマンド + スペース、またはパレット |
| 括弧 | `(` `[` で開き、`)` `]` で外へ出る |
| 行列に行／列を追加 | `Ctrl+Enter` / `&` |
| 入れ子の移動 | `←` `→` `↑` `↓` `Home` `End` `Tab` |
| 構造の削除 | `Backspace`（中身は残して展開） |
| 元に戻す / やり直し | `Ctrl+Z` / `Ctrl+Shift+Z` |

対応構造: 分数、平方根・n 乗根、上付き・下付き、括弧（`()` `[]` `{}` `||`）、総和・総乗・積分・極限、
行列・場合分け、ギリシャ文字と主要記号、`sin` などの関数名。

## 本文の操作

| 操作 | ショートカット |
| --- | --- |
| 新規 / 開く / 保存 / 名前を付けて保存 | `Ctrl+N` / `Ctrl+O` / `Ctrl+S` / `Ctrl+Shift+S` |
| 検索・置換 | `Ctrl+F` |
| HTML（MathML）書き出し | ツールバーの「HTML出力」 |

## 開発

必要なもの: Rust stable、`wasm32-unknown-unknown` ターゲット、[Trunk]、[Tauri CLI] v2。
Linux では `libwebkit2gtk-4.1-dev` などの Tauri 依存パッケージも必要です。

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked trunk
cargo install --locked tauri-cli --version "^2"

cargo tauri dev      # 開発起動
cargo test           # 数式エンジンとファイル形式のテスト
cargo tauri build    # インストーラの生成
```

起動時間を測る場合は `MATHNOTE_STARTUP_LOG=1` を設定して起動すると、最初の描画までの ms が
標準エラーに出力されます。

## 自作エンジンの制限

LaTeX の解釈は上記の対応構造に限られます。未知のコマンドは文字として保持されるため壊れませんが、
組版はされません。細かな組版規則（イタリック補正、記号ごとのスペーシング）も簡略化しています。

[Trunk]: https://trunkrs.dev/
[Tauri CLI]: https://tauri.app/
