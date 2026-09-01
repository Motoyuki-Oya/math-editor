//! ドキュメントの描画方法。
//!
//! [`crate::structure`] のみに依存します。 [`crate::format`] に到達してはなりません。画面上の何も表記法から派生することはできません。そのため、パレット、ツールチップ、およびキャレットはすべて、代わりに構造の観点から機能します。
//!
//! [`row`] は、あらゆる深さで行を描画する 1 つのコンポーネントです。 [`document`] は行をまとめ、[`measure`] は行が到着した位置を読み戻して、キャレットを配置してクリックを理解できるようにします。

pub mod document;
mod heights;
pub mod measure;
pub mod row;
pub(crate) mod viewport;
