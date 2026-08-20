# Planetext GPUI + WebView 版

このディレクトリは、既存の Leptos/Tauri フロントエンドを GPUI ウィンドウ内の Wry WebView でホストする実験的な別実装です。
Tauri 版と同じ `dist/` 資産を読み込み、Tauri の `__TAURI__.core.invoke` API をエミュレートしてバックエンド機能を提供します。

## 構成

- `src/main.rs` — GPUI アプリケーションとウィンドウの起動
- `src/webview.rs` — Wry WebView を GPUI 要素として配置し、サイズを同期
- `src/protocol.rs` — カスタムプロトコル `planetext://` で `dist/` を配信
- `src/bridge.rs` — `window.ipc.postMessage` 経由の呼び出しを処理し、ファイルダイアログ・設定・下書き・文書入出力を行う

## ビルド

```bash
cd gpui
cargo build --release
```

またはワークスペースルートから：

```bash
cargo build -p planetext-gpui --release
```

リリース時は `trunk build` または Tauri のフロントエンドビルドで `dist/` を最新にしてから `cargo build` してください。

## 実行

```bash
cargo run -p planetext-gpui
```

`dist/` が `../dist` に存在し、少なくとも `index.html`、CSS、WASM、JS が含まれている必要があります。

## 制限・既知の問題

- **Linux GPU**: GPUI は Vulkan/Metal/DirectX ベースのレンダラを使用するため、GPU ドライバーまたは適切な Vulkan 実装がない環境では起動しないことがあります。`mesa-vulkan-drivers` などのソフトウェア実装(Lavapipe)で動かす場合があります。
- **Linux GTK ループ**: wry(WebKitGTK) は GTK イベントループを必要とします。現状では起動時に `gtk::init()` を呼んでいますが、WebView の描画更新が不安定な場合は、GPUI のメインループ内で `gtk::main_iteration_do(false)` を回す必要があるかもしれません。
- **メニュー**: GPUI 版にはまだネイティブ OS メニューがありません。フロントエンドの UI からのファイル操作はダイアログ経由で動作します。
- **WebView の重ね描き**: Wry WebView はウィンドウの最前面にレンダリングされるため、GPUI 側で WebView の上に他の GPUI 要素を重ねることはできません。本実装では WebView をウィンドウ全体に配置しています。
