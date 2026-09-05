//! エディター コア: ブラウザーに「contenteditable」要素を編集させる代わりに、エディター自体がレンダリングするドキュメント。キーボードと IME の入力は、隠しテキストエリアを介して届きます。これにより、複数のカーソルが可能になります。

pub mod clipboard;
mod commands;
mod input;
mod keys;
pub mod model;
mod mouse;
pub mod search;
pub mod segment;
mod session;
pub mod suggest;
mod trigger;

#[allow(unused_imports)]
pub use commands::{
    annotate, apply_far_match, current_cursor_pos, current_cursor_pos_pane, current_match_number,
    current_match_number_pane, far_search_start, far_search_start_pane, find_far_in_line,
    find_next, find_next_pane, find_next_resident, find_next_resident_pane, find_previous,
    find_previous_pane, find_previous_resident, find_previous_resident_pane, insert_node,
    replace_all, replace_and_find_next, replace_and_find_next_pane, select_all, FarCopy,
};
pub use search::SearchOptions;
#[allow(unused_imports)]
pub use session::{
    add_on_redraw, apply_flush_to_other_panes, apply_restored, bind_doc, clear_modified,
    clear_modified_doc, clear_search_preview, clear_search_preview_pane, close_pane,
    doc_modified_lines, feed_pane, focus_pane, fully_resident, get_or_create_doc_model, init,
    is_focused_overwrite_mode, load_doc_contents, load_pending, load_pending_doc, load_sparse,
    load_sparse_doc, mark_doc_all_modified, pane_count, preview_search_pane, redraw_all,
    redraw_doc, release_doc, reset_all_overwrite_modes, session, set_doc_file_size,
    set_doc_modified_lines, set_doc_path, set_line_count, set_on_change, set_on_far_copy,
    set_on_focus, set_on_missing, set_on_tail, show_tail, stats, take_flush, toggle_overwrite_mode,
    url_at_caret, DocStats, FlushBatch, UrlTooltip,
};
