//! ドキュメントがどのようにファイルに書き込まれ、また読み取られるか。
//!
//! [`crate::structure`] のみに依存します。 [`crate::view`] には決して到達してはなりません。また、ここにあるものは DOM に触れてはなりません。記法は画面なしで読み取りおよび書き込み可能でなければなりません。

pub mod document;
pub mod islands;
pub mod notation;
