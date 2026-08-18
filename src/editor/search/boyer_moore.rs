//! リテラル検索に使う Boyer-Moore 法を提供します。

use std::collections::HashMap;

/// 事前計算した表を使ってリテラル検索を行います。
#[derive(Debug)]
pub(super) struct BoyerMoore {
    pattern: Vec<char>,
    case_sensitive: bool,
    bad_char: HashMap<char, usize>,
    good_suffix: Vec<usize>,
}

impl BoyerMoore {
    /// 検索パターンをコンパイルします。
    pub(super) fn new(query: &str, case_sensitive: bool) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        let pattern: Vec<char> = if case_sensitive {
            query.chars().collect()
        } else {
            query.to_lowercase().chars().collect()
        };
        let mut bad_char = HashMap::new();
        for (i, c) in pattern.iter().enumerate() {
            bad_char.insert(*c, i);
        }
        let good_suffix = build_good_suffix(&pattern);
        Some(Self {
            pattern,
            case_sensitive,
            bad_char,
            good_suffix,
        })
    }

    /// 文字列内の一致範囲を文字インデックスで返します。
    pub(super) fn find(&self, run: &str) -> Vec<(usize, usize)> {
        let m = self.pattern.len();
        if m == 0 {
            return Vec::new();
        }

        let run_chars: Vec<char> = run.chars().collect();
        let (text, mapping) = if self.case_sensitive {
            (run_chars.clone(), None)
        } else {
            let mut text = Vec::with_capacity(run_chars.len());
            let mut map = Vec::with_capacity(run_chars.len());
            for (i, c) in run_chars.iter().enumerate() {
                for lc in c.to_lowercase() {
                    text.push(lc);
                    map.push(i);
                }
            }
            (text, Some(map))
        };

        let n = text.len();
        let mut found = Vec::new();
        let mut s = 0;
        while s + m <= n {
            let mut j: isize = m as isize - 1;
            while j >= 0 && text[(s as isize + j) as usize] == self.pattern[j as usize] {
                j -= 1;
            }
            if j < 0 {
                let (from, to) = if let Some(map) = &mapping {
                    let from = map[s];
                    let to = map[s + m - 1] + 1;
                    (from, to)
                } else {
                    (s, s + m)
                };
                found.push((from, to));
                s += self.good_suffix[0];
            } else {
                let ju = j as usize;
                let c = text[s + ju];
                let k = self
                    .bad_char
                    .get(&c)
                    .copied()
                    .map(|k| k as isize)
                    .unwrap_or(-1);
                let bc = ((ju as isize - k).max(1)) as usize;
                s += bc.max(self.good_suffix[ju]);
            }
        }
        found
    }
}

/// Z-関数に基づく suffix テーブルを作ります。
fn suffixes(p: &[char]) -> Vec<usize> {
    let m = p.len();
    let mut suff = vec![0; m];
    suff[m - 1] = m;
    let mut g = (m - 1) as isize;
    let mut f = (m - 1) as isize;
    for i in (0..m - 1).rev() {
        let i_i = i as isize;
        let offset = i_i + m as isize - 1 - f;
        if i_i > g && offset >= 0 && suff[offset as usize] < (i_i - g) as usize {
            suff[i] = suff[offset as usize];
        } else {
            if i_i < g {
                g = i_i;
            }
            f = i_i;
            while g >= 0 && p[g as usize] == p[(g + m as isize - 1 - f) as usize] {
                g -= 1;
            }
            suff[i] = (f - g) as usize;
        }
    }
    suff
}

/// Boyer-Moore の good-suffix シフトテーブルを構築します。
fn build_good_suffix(p: &[char]) -> Vec<usize> {
    let m = p.len();
    let suff = suffixes(p);
    let mut gs = vec![m; m];
    let mut j = 0;
    for i in (0..m).rev() {
        if suff[i] == i + 1 {
            let target = m - 1 - i;
            while j < target {
                if gs[j] == m {
                    gs[j] = m - 1 - i;
                }
                j += 1;
            }
        }
    }
    for (i, &s) in suff.iter().enumerate().take(m - 1) {
        let index = m - 1 - s;
        if index < m {
            gs[index] = m - 1 - i;
        }
    }
    gs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_builds_bad_character_and_good_suffix_tables() {
        let matcher = BoyerMoore::new("aab", true).unwrap();
        assert_eq!(matcher.bad_char, HashMap::from([('a', 1), ('b', 2)]));
        assert_eq!(matcher.good_suffix, vec![3, 3, 1]);
    }

    #[test]
    fn boyer_moore_finds_overlapping_matches() {
        let matcher = BoyerMoore::new("aa", true).unwrap();
        assert_eq!(matcher.find("aaaa"), vec![(0, 2), (1, 3), (2, 4)]);
    }

    #[test]
    fn finds_matches_at_the_start_and_end() {
        let matcher = BoyerMoore::new("ab", true).unwrap();
        assert_eq!(matcher.find("abxxab"), vec![(0, 2), (4, 6)]);
    }

    #[test]
    fn returns_no_matches_for_absent_or_short_runs() {
        let matcher = BoyerMoore::new("abc", true).unwrap();
        assert!(matcher.find("xyz").is_empty());
        assert!(matcher.find("ab").is_empty());
    }

    #[test]
    fn an_empty_pattern_does_not_compile() {
        assert!(BoyerMoore::new("", true).is_none());
    }

    #[test]
    fn case_insensitive_matching_maps_expanded_lowercase_back_to_original_chars() {
        let matcher = BoyerMoore::new("i\u{307}", false).unwrap();
        assert_eq!(matcher.find("İ"), vec![(0, 1)]);
    }

    #[test]
    fn non_ascii_matches_use_character_indices() {
        let matcher = BoyerMoore::new("日本", true).unwrap();
        assert_eq!(matcher.find("xx日本語"), vec![(2, 4)]);
    }

    #[test]
    fn finds_a_single_character() {
        let matcher = BoyerMoore::new("x", true).unwrap();
        assert_eq!(matcher.find("axbxc"), vec![(1, 2), (3, 4)]);
    }
}
