use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::document::Document;
use crate::piece_tree::Piece;
use crate::source::{FileEncoding, ScanIndex, ScanState, Source, CHUNK, STRIDE};

impl Document {
    /// 文書の行を書き手へ流す。全文を 1 つの文字列に集めない。
    pub(crate) fn write_to<W: Write>(&mut self, out: &mut W) -> Result<(), String> {
        let mut broken = None;
        let line_ending_bytes = self.line_ending.as_bytes();
        let encoding = self.encoding;
        if encoding == FileEncoding::Utf8Bom {
            if let Err(e) = out.write_all(b"\xEF\xBB\xBF") {
                return Err(format!("書き込めませんでした: {e}"));
            }
        }
        self.each_line(0, self.count, &mut |i, line| {
            let write = |out: &mut W| -> std::io::Result<()> {
                if i > 0 {
                    out.write_all(line_ending_bytes)?;
                }
                let clean_line = line.trim_end_matches(['\r', '\n']);
                out.write_all(&encoding.encode_str(clean_line))
            };
            match write(out) {
                Ok(()) => true,
                Err(e) => {
                    broken = Some(format!("書き込めませんでした: {e}"));
                    false
                }
            }
        })?;
        match broken {
            Some(error) => Err(error),
            None => out
                .flush()
                .map_err(|e| format!("書き込めませんでした: {e}")),
        }
    }

    /// 文書をディスクへ流し、保存したファイルを新しい本体にする。
    /// 一時ファイルへ書いてから入れ替えるので、書きかけで元を壊さない。
    pub(crate) fn save(&mut self, path: &str) -> Result<(), String> {
        let tmp = format!("{path}.saving");
        let fail = |e: String| format!("{path} を保存できませんでした: {e}");
        // 書きながら次の索引を作る。保存が終わった姿はこの索引そのもの。
        let initial_offset = if self.encoding == FileEncoding::Utf8Bom {
            3
        } else {
            0
        };
        let mut marks = vec![initial_offset];
        let mut written = 0;
        {
            let file = File::create(&tmp).map_err(|e| fail(e.to_string()))?;
            let mut out = BufWriter::with_capacity(CHUNK, file);
            if self.encoding == FileEncoding::Utf8Bom {
                out.write_all(b"\xEF\xBB\xBF")
                    .map_err(|e| fail(e.to_string()))?;
                written += 3;
            }
            let count = self.count;
            let mut broken = None;
            let line_ending_bytes = self.line_ending.as_bytes();
            let encoding = self.encoding;
            self.each_line(0, count, &mut |i, line| {
                let mut write = |out: &mut BufWriter<File>| -> std::io::Result<()> {
                    if i > 0 {
                        out.write_all(line_ending_bytes)?;
                        written += line_ending_bytes.len() as u64;
                        if i % STRIDE == 0 {
                            marks.push(written);
                        }
                    }
                    let clean_line = line.trim_end_matches(['\r', '\n']);
                    let encoded = encoding.encode_str(clean_line);
                    out.write_all(&encoded)?;
                    written += encoded.len() as u64;
                    Ok(())
                };
                match write(&mut out) {
                    Ok(()) => true,
                    Err(e) => {
                        broken = Some(e.to_string());
                        false
                    }
                }
            })?;
            if let Some(error) = broken {
                std::fs::remove_file(&tmp).ok();
                return Err(fail(error));
            }
            out.flush().map_err(|e| fail(e.to_string()))?;
        }
        // 自分が読んでいる元ファイルへ重ねる場合だけ、rename の直前に手を放す。
        // rename が失敗したら必ず戻す。Disk piece を残したまま Source だけ失うと、
        // その後の読みがパニックし、文書マップの Mutex まで poison される。
        let replacing_source = self
            .source
            .as_ref()
            .is_some_and(|source| source.path == Path::new(path));
        let old_source = replacing_source.then(|| self.source.take()).flatten();
        if let Err(error) = std::fs::rename(&tmp, path) {
            self.source = old_source;
            std::fs::remove_file(&tmp).ok();
            return Err(fail(error.to_string()));
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) => {
                self.source = old_source;
                return Err(fail(error.to_string()));
            }
        };
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        self.source = Some(Source {
            path: Path::new(path).to_path_buf(),
            file,
            index: Arc::new(ScanIndex {
                state: Mutex::new(ScanState {
                    marks,
                    lines: self.count,
                    done: true,
                    broken: None,
                }),
            }),
            bytes: written,
            modified,
            encoding: self.encoding,
            line_ending: self.line_ending,
        });
        self.pieces = vec![Piece::Disk {
            from: 0,
            lines: self.count,
        }];
        self.log.saved_undo_len = self.log.undo.len();
        Ok(())
    }
}
