//! テキストの意味: 通常の文字と、文字だけでは保持できない 2 次元構造。
//!
//! このレイヤーは、[`crate::format`] と [`crate::view`] が共有する唯一のものであるため、どちらにも依存してはなりません。構造がどのようにファイルに書き込まれるか、またどのように描画されるかについては何も知りません。このままにしておくと、表記と表示が互いに接触することなく変化します (`docs/architecture.md` を参照)。

pub mod ast;
pub mod edit;
pub mod plain;
pub mod text;
pub mod vocabulary;
