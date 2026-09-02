use std::collections::HashMap;

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

/// オンデマンド bi-gram 検索索引。
/// 巨大ファイルでも全文一括読み込みを行わず、512KB ブロック単位でオンデマンドに構築・管理する。
#[derive(Clone, Debug)]
pub(crate) struct SearchIndex {
    block_indices: HashMap<BlockId, HashMap<Bigram, u32>>,
    total_blocks: usize,
    pub(crate) deltas: DeltaCache,
    pub(crate) encoding: FileEncoding,
    pub(crate) total_bytes: usize,
}

impl SearchIndex {
    pub(crate) fn new(total_bytes: usize, encoding: FileEncoding) -> Self {
        let total_blocks = total_bytes.div_ceil(INDEX_BLOCK_BYTES);
        Self {
            block_indices: HashMap::new(),
            total_blocks,
            deltas: DeltaCache::default(),
            encoding,
            total_bytes,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn total_blocks(&self) -> usize {
        self.total_blocks
    }

    /// 指定ブロックのベース索引をオンデマンドで構築（全文一括読み込みは行わない）
    pub(crate) fn ensure_block(
        &mut self,
        block: BlockId,
        source: &mut Source,
    ) -> Result<(), String> {
        if self.block_indices.contains_key(&block) || block >= self.total_blocks {
            return Ok(());
        }
        let start_byte = block * INDEX_BLOCK_BYTES;
        let read_start = if block > 0 && start_byte >= 4 {
            start_byte - 4
        } else {
            start_byte
        };
        let end_byte = ((block + 1) * INDEX_BLOCK_BYTES).min(self.total_bytes);
        if read_start >= end_byte {
            return Ok(());
        }
        let len = end_byte - read_start;
        let bytes = source.read_byte_range(source.content_offset as usize + read_start, len)?;
        let text = self.encoding.decode_line(&bytes);
        let mut counts = HashMap::new();
        let mut previous: Option<char> = None;
        let mut current_offset = read_start;

        for ch in text.chars() {
            let ch_len = self.encoding.encode_str(&ch.to_string()).len();
            if let Some(prev) = previous {
                // 境界またぎ bi-gram は終了側ブロックに所属
                if current_offset >= start_byte && current_offset < end_byte {
                    if let Some(bg) = Bigram::new(prev, ch, self.encoding) {
                        *counts.entry(bg).or_insert(0) += 1;
                    }
                }
            }
            previous = Some(ch);
            current_offset += ch_len;
        }

        self.block_indices.insert(block, counts);
        Ok(())
    }

    /// クエリに含まれる bi-gram から、特定ブロックでの推定件数を計算
    pub(crate) fn estimate_block_matches(
        &mut self,
        block: BlockId,
        query: &str,
        source: &mut Source,
    ) -> Result<Option<usize>, String> {
        let bigrams = Bigram::from_query(query, self.encoding);
        if bigrams.is_empty() {
            return Ok(None);
        }
        self.ensure_block(block, source)?;
        let Some(counts) = self.block_indices.get(&block) else {
            return Ok(None);
        };

        let mut min_count = i64::MAX;
        for bg in &bigrams {
            let base = counts.get(bg).copied().unwrap_or(0) as i64;
            let delta = self.deltas.delta(bg, block);
            let net = (base + delta).max(0);
            min_count = min_count.min(net);
        }
        Ok(Some(if min_count == i64::MAX {
            0
        } else {
            min_count as usize
        }))
    }

    /// 全ブロックにわたる推定件数を計算
    pub(crate) fn estimate_matches(
        &mut self,
        query: &str,
        source: &mut Source,
    ) -> Result<Option<usize>, String> {
        if self.total_blocks == 0 {
            return Ok(Some(0));
        }
        let bigrams = Bigram::from_query(query, self.encoding);
        if bigrams.is_empty() {
            return Ok(None);
        }
        let mut total = 0;
        for block in 0..self.total_blocks {
            if let Some(count) = self.estimate_block_matches(block, query, source)? {
                total += count;
            }
        }
        Ok(Some(total))
    }

    /// 編集発生時に差分キャッシュを更新
    pub(crate) fn splice_delta(&mut self, before: &str, after: &str, byte_pos: usize) {
        let block = byte_pos / INDEX_BLOCK_BYTES;
        self.deltas.splice(before, after, self.encoding, block);
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
