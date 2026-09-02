use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::source::{FileEncoding, Source};

pub(crate) const INDEX_BLOCK_BYTES: usize = 512 * 1024;
pub(crate) const BIGRAM_INDEX_THRESHOLD: usize = 10_000_000;
pub(crate) type BlockId = usize;

/// エンコーディングに依存したバイト列による bi-gram。
/// 構造フォーマットや表記法をフィルタリングせず、文書の生テキストからそのまま生成する。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Bigram(pub(crate) Vec<u8>);

impl Bigram {
    pub(crate) fn new(first: char, second: char, encoding: FileEncoding) -> Option<Self> {
        let mut bytes = encoding.encode_str(&first.to_string());
        bytes.extend_from_slice(&encoding.encode_str(&second.to_string()));
        (!bytes.is_empty()).then_some(Self(bytes))
    }

    pub(crate) fn from_query(query: &str, encoding: FileEncoding) -> Vec<Self> {
        let chars: Vec<_> = query.chars().collect();
        chars
            .windows(2)
            .filter_map(|w| Self::new(w[0], w[1], encoding))
            .collect()
    }

    pub(crate) fn variants_from_query(
        query: &str,
        case_sensitive: bool,
        encoding: FileEncoding,
    ) -> Vec<Vec<Self>> {
        let chars: Vec<_> = query.chars().collect();
        chars
            .windows(2)
            .filter_map(|w| {
                if case_sensitive {
                    Self::new(w[0], w[1], encoding).map(|b| vec![b])
                } else {
                    let mut variants = Vec::new();
                    let first_opts: Vec<char> = if w[0].is_alphabetic() {
                        w[0].to_lowercase().chain(w[0].to_uppercase()).collect()
                    } else {
                        vec![w[0]]
                    };
                    let second_opts: Vec<char> = if w[1].is_alphabetic() {
                        w[1].to_lowercase().chain(w[1].to_uppercase()).collect()
                    } else {
                        vec![w[1]]
                    };
                    for &c1 in &first_opts {
                        for &c2 in &second_opts {
                            if let Some(b) = Self::new(c1, c2, encoding) {
                                if !variants.contains(&b) {
                                    variants.push(b);
                                }
                            }
                        }
                    }
                    (!variants.is_empty()).then_some(variants)
                }
            })
            .collect()
    }
}

/// 編集によって生じた bi-gram の正負のネット件数差分キャッシュ。
#[derive(Clone, Debug, Default)]
pub(crate) struct DeltaCache {
    counts: HashMap<(Bigram, BlockId), i64>,
}

impl DeltaCache {
    pub(crate) fn add(&mut self, bigram: Bigram, block: BlockId, amount: i64) {
        let entry = self.counts.entry((bigram, block)).or_default();
        *entry += amount;
        if *entry == 0 {
            self.counts.retain(|_, v| *v != 0);
        }
    }

    pub(crate) fn splice(
        &mut self,
        before: &str,
        after: &str,
        encoding: FileEncoding,
        block: BlockId,
    ) {
        for b in Bigram::from_query(before, encoding) {
            self.add(b, block, -1);
        }
        for b in Bigram::from_query(after, encoding) {
            self.add(b, block, 1);
        }
    }

    pub(crate) fn delta(&self, bigram: &Bigram, block: BlockId) -> i64 {
        self.counts
            .get(&(bigram.clone(), block))
            .copied()
            .unwrap_or(0)
    }

    #[allow(dead_code)]
    pub(crate) fn clear(&mut self) {
        self.counts.clear();
    }
}

#[derive(Debug)]
pub(crate) struct SearchIndexState {
    pub(crate) block_indices: HashMap<BlockId, HashMap<Bigram, u32>>,
    pub(crate) total_blocks: usize,
    pub(crate) deltas: DeltaCache,
    pub(crate) encoding: FileEncoding,
    pub(crate) total_bytes: usize,
}

/// バックグラウンドで非同期にインデックスを構築するワーカー。
/// UIスレッドやドキュメント操作を邪魔せず、「こっそり」ブロックごとに進める。
pub(crate) struct BackgroundIndex {
    pub(crate) path: PathBuf,
    pub(crate) content_offset: u64,
    pub(crate) encoding: FileEncoding,
    pub(crate) total_bytes: usize,
    pub(crate) state: Arc<RwLock<SearchIndexState>>,
}

impl BackgroundIndex {
    pub(crate) fn run(self) {
        let Ok(file) = File::open(&self.path) else {
            return;
        };
        let mut reader = BufReader::new(file);
        let total_blocks = self.total_bytes.div_ceil(INDEX_BLOCK_BYTES);
        for block in 0..total_blocks {
            // ドキュメントが閉じられたら速やかに終了
            if Arc::strong_count(&self.state) == 1 {
                return;
            }
            {
                let state = self.state.read().unwrap();
                if state.block_indices.contains_key(&block) {
                    continue;
                }
            }
            let start_byte = block * INDEX_BLOCK_BYTES;
            let read_start = if block > 0 && start_byte >= 4 {
                start_byte - 4
            } else {
                start_byte
            };
            let end_byte = ((block + 1) * INDEX_BLOCK_BYTES).min(self.total_bytes);
            if read_start >= end_byte {
                continue;
            }
            let len = end_byte - read_start;
            if reader
                .seek(SeekFrom::Start(self.content_offset + read_start as u64))
                .is_err()
            {
                return;
            }
            let mut buf = vec![0u8; len];
            if reader.read_exact(&mut buf).is_err() {
                return;
            }
            let counts = SearchIndex::extract_block_bigrams(
                &buf,
                read_start,
                start_byte,
                end_byte,
                self.encoding,
            );
            {
                let mut state = self.state.write().unwrap();
                state.block_indices.insert(block, counts);
            }
            // CPUやディスクI/Oを占有しないよう、ブロック間に休止を入れる
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

/// オンデマンド bi-gram 検索索引。
/// 巨大ファイルでも全文一括読み込みを行わず、512KB ブロック単位でオンデマンド・非同期に構築・管理する。
#[derive(Clone, Debug)]
pub(crate) struct SearchIndex {
    pub(crate) state: Arc<RwLock<SearchIndexState>>,
}

impl SearchIndex {
    pub(crate) fn new(total_bytes: usize, encoding: FileEncoding) -> Self {
        let total_blocks = total_bytes.div_ceil(INDEX_BLOCK_BYTES);
        Self {
            state: Arc::new(RwLock::new(SearchIndexState {
                block_indices: HashMap::new(),
                total_blocks,
                deltas: DeltaCache::default(),
                encoding,
                total_bytes,
            })),
        }
    }

    pub(crate) fn extract_block_bigrams(
        bytes: &[u8],
        read_start: usize,
        start_byte: usize,
        end_byte: usize,
        encoding: FileEncoding,
    ) -> HashMap<Bigram, u32> {
        let text = encoding.decode_line(bytes);
        let mut counts = HashMap::new();
        let mut previous: Option<char> = None;
        let mut current_offset = read_start;

        for ch in text.chars() {
            let ch_len = encoding.encode_str(&ch.to_string()).len();
            if let Some(prev) = previous {
                // 境界またぎ bi-gram は終了側ブロックに所属
                if current_offset >= start_byte && current_offset < end_byte {
                    if let Some(bg) = Bigram::new(prev, ch, encoding) {
                        *counts.entry(bg).or_insert(0) += 1;
                    }
                }
            }
            previous = Some(ch);
            current_offset += ch_len;
        }
        counts
    }

    /// テストや即時構築用: 指定ブロックのベース索引を同期的に構築
    pub(crate) fn ensure_block(&self, block: BlockId, source: &mut Source) -> Result<(), String> {
        {
            let state = self.state.read().unwrap();
            if state.block_indices.contains_key(&block) || block >= state.total_blocks {
                return Ok(());
            }
        }
        let (total_bytes, encoding) = {
            let state = self.state.read().unwrap();
            (state.total_bytes, state.encoding)
        };
        let start_byte = block * INDEX_BLOCK_BYTES;
        let read_start = if block > 0 && start_byte >= 4 {
            start_byte - 4
        } else {
            start_byte
        };
        let end_byte = ((block + 1) * INDEX_BLOCK_BYTES).min(total_bytes);
        if read_start >= end_byte {
            return Ok(());
        }
        let len = end_byte - read_start;
        let bytes = source.read_byte_range(source.content_offset as usize + read_start, len)?;
        let counts =
            Self::extract_block_bigrams(&bytes, read_start, start_byte, end_byte, encoding);
        let mut state = self.state.write().unwrap();
        state.block_indices.insert(block, counts);
        Ok(())
    }

    /// 全ブロックを同期構築する（主に単体テスト用）
    #[allow(dead_code)]
    pub(crate) fn ensure_all_blocks(&self, source: &mut Source) -> Result<(), String> {
        let total_blocks = self.state.read().unwrap().total_blocks;
        for b in 0..total_blocks {
            self.ensure_block(b, source)?;
        }
        Ok(())
    }

    /// 構築済みブロックのみを使って推定件数を計算する。
    /// 未構築ブロックについては同期読み込みを行わず、現在のカバレッジから全体件数を按分推定する。
    /// 「出来たところから使ってほしい」の仕様を満たす。
    pub(crate) fn estimate_matches(&self, query: &str) -> Option<usize> {
        let state = self.state.read().unwrap();
        if state.total_blocks == 0 {
            return Some(0);
        }
        let bigrams = Bigram::from_query(query, state.encoding);
        if bigrams.is_empty() {
            return None;
        }
        let indexed_blocks = state.block_indices.len();
        if indexed_blocks == 0 {
            return None; // まだ1ブロックも出来ていないのでサンプリング推定へフォールバック
        }

        let mut indexed_total: usize = 0;
        for (&block, counts) in &state.block_indices {
            let mut min_count = i64::MAX;
            for bg in &bigrams {
                let base = counts.get(bg).copied().unwrap_or(0) as i64;
                let delta = state.deltas.delta(bg, block);
                let net = (base + delta).max(0);
                min_count = min_count.min(net);
            }
            if min_count != i64::MAX {
                indexed_total += min_count as usize;
            }
        }

        // 出来たブロックの比率で全体へ按分推定
        let estimated =
            (indexed_total as u128 * state.total_blocks as u128 / indexed_blocks as u128) as usize;
        Some(estimated)
    }

    /// 編集発生時に差分キャッシュを更新
    pub(crate) fn splice_delta(&self, before: &str, after: &str, byte_pos: usize) {
        let mut state = self.state.write().unwrap();
        let encoding = state.encoding;
        let block = byte_pos / INDEX_BLOCK_BYTES;
        state.deltas.splice(before, after, encoding, block);
    }

    /// `from_byte` 以降で、クエリの候補が存在しうる論理ブロックの開始バイト位置を返す。
    /// 現在のブロックに候補がなければ、後続ブロックの索引を調べて最初に候補が存在しうるブロックへジャンプする。
    /// - 未インデックスのブロック: 候補ありとみなしてそのブロックの開始位置を返す（見逃し防止）
    /// - インデックス済みのブロック: クエリの全 bi-gram window のうち1つでもカウントが0ならスキップ
    pub(crate) fn next_candidate_byte(
        &self,
        from_byte: usize,
        query: &str,
        case_sensitive: bool,
    ) -> usize {
        let state = self.state.read().unwrap();
        let total_bytes = state.total_bytes;
        let total_blocks = state.total_blocks;
        if total_blocks == 0 || from_byte >= total_bytes {
            return from_byte;
        }
        let windows = Bigram::variants_from_query(query, case_sensitive, state.encoding);
        if windows.is_empty() {
            return from_byte;
        }

        let start_block = from_byte / INDEX_BLOCK_BYTES;
        for block in start_block..total_blocks {
            if let Some(counts) = state.block_indices.get(&block) {
                let mut has_all = true;
                for variants in &windows {
                    let mut sum_count = 0i64;
                    for bg in variants {
                        let base = counts.get(bg).copied().unwrap_or(0) as i64;
                        let delta = state.deltas.delta(bg, block);
                        sum_count += (base + delta).max(0);
                    }
                    if sum_count == 0 {
                        has_all = false;
                        break;
                    }
                }
                if has_all {
                    let block_start = block * INDEX_BLOCK_BYTES;
                    return from_byte.max(block_start);
                }
            } else {
                // 未インデックスのブロックは候補ありとみなす（出来たところから使う）
                let block_start = block * INDEX_BLOCK_BYTES;
                return from_byte.max(block_start);
            }
        }
        total_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigram_creation_and_query_extraction() {
        let bigrams = Bigram::from_query("hello", FileEncoding::Utf8);
        assert_eq!(bigrams.len(), 4);
    }

    #[test]
    fn delta_cache_accumulates_and_clears() {
        let mut deltas = DeltaCache::default();
        let bg = Bigram::new('a', 'b', FileEncoding::Utf8).unwrap();
        deltas.add(bg.clone(), 0, 3);
        assert_eq!(deltas.delta(&bg, 0), 3);

        deltas.add(bg.clone(), 0, -3);
        assert_eq!(deltas.delta(&bg, 0), 0);
    }

    #[test]
    fn delta_cache_splice_tracks_changes() {
        let mut deltas = DeltaCache::default();
        let bg_old = Bigram::new('a', 'b', FileEncoding::Utf8).unwrap();
        let bg_new = Bigram::new('c', 'd', FileEncoding::Utf8).unwrap();

        deltas.splice("ab", "cd", FileEncoding::Utf8, 0);
        assert_eq!(deltas.delta(&bg_old, 0), -1);
        assert_eq!(deltas.delta(&bg_new, 0), 1);
    }
}
