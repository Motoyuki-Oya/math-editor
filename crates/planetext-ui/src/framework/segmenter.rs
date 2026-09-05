//! Web / WASM 環境における単語分割プロバイダ（Intl.Segmenter 実装）。
//! GUIフレームワーク・ブラウザ接続層として、モデル層の抽象界面 `WordSegmenter` を実装します。

#[cfg(target_arch = "wasm32")]
mod web_impl {
    use crate::editor::segment::WordSegmenter;

    #[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
let segmenter = null;
try {
    if (typeof Intl !== 'undefined' && Intl.Segmenter) {
        segmenter = new Intl.Segmenter(undefined, { granularity: 'word' });
    }
} catch (e) {
    segmenter = null;
}

export function segment_word_at_js(text, char_index) {
    if (!segmenter) return null;
    try {
        const chars = Array.from(text);
        if (char_index >= chars.length) return null;

        const isKatakana = (c) => /[\u30A0-\u30FF\u31F0-\u31FF\uFF65-\uFF9F\u30FC\u30FB]/.test(c);
        const isIdentChar = (c) => /[a-zA-Z0-9_]/.test(c) || /[\uFF10-\uFF19\uFF21-\uFF3A\uFF41-\uFF5A]/.test(c);

        // 1. カタカナ連続（長音符「ー」や中黒「・」を含む）を最優先でひと続きに選択
        if (isKatakana(chars[char_index])) {
            let start = char_index;
            let end = char_index + 1;
            while (start > 0 && isKatakana(chars[start - 1])) {
                start--;
            }
            while (end < chars.length && isKatakana(chars[end])) {
                end++;
            }
            return [start, end];
        }

        // 2. 英数・アンダースコア識別子の連続を最優先で選択
        if (isIdentChar(chars[char_index])) {
            let start = char_index;
            let end = char_index + 1;
            while (start > 0 && isIdentChar(chars[start - 1])) {
                start--;
            }
            while (end < chars.length && isIdentChar(chars[end])) {
                end++;
            }
            return [start, end];
        }

        // 3. 形態素解析 (Intl.Segmenter) による単語境界判定（漢字＋送り仮名等）
        const segments = segmenter.segment(text);
        let cur_idx = 0;
        let prev_seg = null;
        let target_seg = null;

        for (const seg of segments) {
            const char_len = Array.from(seg.segment).length;
            const start = cur_idx;
            const end = start + char_len;
            const item = { start, end, isWord: seg.isWordLike, segment: seg.segment };
            if (char_index >= start && char_index < end) {
                target_seg = item;
                break;
            }
            prev_seg = item;
            cur_idx = end;
        }

        if (target_seg) {
            if (!target_seg.isWord && prev_seg && prev_seg.isWord && char_index === target_seg.start) {
                target_seg = prev_seg;
            }
            let start = target_seg.start;
            let end = target_seg.end;
            if (isKatakana(chars[start])) {
                while (start > 0 && isKatakana(chars[start - 1])) {
                    start--;
                }
                while (end < chars.length && isKatakana(chars[end])) {
                    end++;
                }
            } else if (isIdentChar(chars[start])) {
                while (start > 0 && isIdentChar(chars[start - 1])) {
                    start--;
                }
                while (end < chars.length && isIdentChar(chars[end])) {
                    end++;
                }
            }
            return [start, end];
        }
    } catch (e) {
        return null;
    }
    return null;
}
"#)]
    extern "C" {
        fn segment_word_at_js(text: &str, char_index: usize) -> wasm_bindgen::JsValue;
    }

    pub struct WebWordSegmenter;

    impl WordSegmenter for WebWordSegmenter {
        fn segment_word(&self, text: &str, char_index: usize) -> Option<(usize, usize)> {
            let val = segment_word_at_js(text, char_index);
            if val.is_array() {
                let arr = js_sys::Array::from(&val);
                if arr.length() == 2 {
                    let start = arr.get(0).as_f64()? as usize;
                    let end = arr.get(1).as_f64()? as usize;
                    return Some((start, end));
                }
            }
            None
        }
    }
}

/// Web 環境の単語分割プロバイダを初期化し、エディタモデル層へ登録します。
#[allow(dead_code)]
pub fn init_segmenter() {
    #[cfg(target_arch = "wasm32")]
    {
        crate::editor::segment::set_word_segmenter(Box::new(web_impl::WebWordSegmenter));
    }
}
