//! 複数ファイル対応トランザクションジャーナル（FileTransaction）。
//!
//! 単一ファイルおよび複数ファイルの更新を All-or-Nothing（原子性）で実行し、
//! 書き込み中の中断・クラッシュ時にも次回起動時に安全にロールバック・原状回復する。

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const JOURNAL_NAME: &str = ".tx_journal";

#[derive(Debug)]
struct TransactionEntry {
    target: PathBuf,
    temp: PathBuf,
    backup: PathBuf,
    staged: bool,
    backed_up: bool,
}

/// 複数ファイルの原子的コミットとクラッシュリカバリーを保証するトランザクション。
pub struct FileTransaction {
    journal_path: PathBuf,
    journal_file: Option<File>,
    entries: Vec<TransactionEntry>,
    committed: bool,
}

impl FileTransaction {
    /// 指定ディレクトリをトランザクション基点として開始する。
    /// 既存の未完了ジャーナルが残存している場合は、開始前に自動でロールバック・リカバリーを行う。
    pub fn begin<P: AsRef<Path>>(dir: P) -> Result<Self, String> {
        let dir = dir.as_ref();
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("ディレクトリを作成できませんでした: {e}"))?;
        }

        // 残存ジャーナルがあれば復旧
        Self::recover(dir)?;

        let journal_path = dir.join(JOURNAL_NAME);
        let journal_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&journal_path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "別のトランザクションが実行中です: {}",
                        journal_path.display()
                    )
                } else {
                    format!("ジャーナルファイルを作成できませんでした: {e}")
                }
            })?;

        Ok(Self {
            journal_path,
            journal_file: Some(journal_file),
            entries: Vec::new(),
            committed: false,
        })
    }

    /// トランザクションに対象ファイルを追加し、一時ファイルへ新内容を書き出す。
    pub fn add_file<P, F>(&mut self, target: P, write_fn: F) -> Result<(), String>
    where
        P: AsRef<Path>,
        F: FnOnce(&mut BufWriter<File>) -> Result<(), String>,
    {
        let target = target.as_ref().to_path_buf();
        let file_name = target
            .file_name()
            .ok_or_else(|| "無効なファイル名です".to_string())?
            .to_string_lossy();
        let parent = target
            .parent()
            .ok_or_else(|| "無効な親ディレクトリです".to_string())?;

        let temp = parent.join(format!("{file_name}.tmp"));
        let backup = parent.join(format!("{file_name}.bak"));

        // 以前の残存 .tmp や .bak があれば事前に除去
        let _ = std::fs::remove_file(&temp);
        let _ = std::fs::remove_file(&backup);

        // 一時ファイルへ書き込み
        let file =
            File::create(&temp).map_err(|e| format!("一時ファイルを作成できませんでした: {e}"))?;
        let mut writer = BufWriter::new(file);
        write_fn(&mut writer)?;
        writer
            .flush()
            .map_err(|e| format!("一時ファイルのフラッシュに失敗しました: {e}"))?;

        // ジャーナルにエントリを追記
        if let Some(ref mut jf) = self.journal_file {
            writeln!(
                jf,
                "{}\t{}\t{}",
                target.to_string_lossy(),
                backup.to_string_lossy(),
                temp.to_string_lossy()
            )
            .map_err(|e| format!("ジャーナルへの書き込みに失敗しました: {e}"))?;
            jf.flush()
                .map_err(|e| format!("ジャーナルのフラッシュに失敗しました: {e}"))?;
        }

        self.entries.push(TransactionEntry {
            target,
            temp,
            backup,
            staged: true,
            backed_up: false,
        });

        Ok(())
    }

    /// バイト列を直接指定してファイルを追加するユーティリティ。
    pub fn add_file_bytes<P: AsRef<Path>>(
        &mut self,
        target: P,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.add_file(target, |writer| {
            writer
                .write_all(bytes)
                .map_err(|e| format!("書き込みに失敗しました: {e}"))
        })
    }

    /// 全ファイルを一括でアトミックに確定（コミット）する。
    /// 途中でエラーが発生した場合は自動的にロールバックして元に戻す。
    pub fn commit(mut self) -> Result<(), String> {
        // Step 1: 各既存ファイルを .bak へ退避
        for entry in &mut self.entries {
            if entry.target.exists() {
                let _ = std::fs::remove_file(&entry.backup);
                if let Err(e) = std::fs::rename(&entry.target, &entry.backup) {
                    self.rollback();
                    return Err(format!("バックアップの作成に失敗しました: {e}"));
                }
                entry.backed_up = true;
            }
        }

        // Step 2: 各 .tmp を本番ターゲットへ rename
        for entry in &mut self.entries {
            let _ = std::fs::remove_file(&entry.target);
            if let Err(e) = std::fs::rename(&entry.temp, &entry.target) {
                self.rollback();
                return Err(format!("本番ファイルへの置換に失敗しました: {e}"));
            }
            entry.staged = false;
        }

        // Step 3: すべて成功したため、バックアップ群を削除
        for entry in &self.entries {
            let _ = std::fs::remove_file(&entry.backup);
        }

        // Step 4: ジャーナルファイルをクローズして削除
        self.journal_file = None;
        let _ = std::fs::remove_file(&self.journal_path);
        self.committed = true;

        Ok(())
    }

    /// 未完了の変更を取り消し、バックアップから原状回復する。
    pub fn rollback(&mut self) {
        // 本番ファイルになってしまったものをバックアップから戻す
        for entry in &mut self.entries {
            if entry.backed_up && entry.backup.exists() {
                let _ = std::fs::remove_file(&entry.target);
                let _ = std::fs::rename(&entry.backup, &entry.target);
                entry.backed_up = false;
            }
            // 残存する .tmp を削除
            if entry.temp.exists() {
                let _ = std::fs::remove_file(&entry.temp);
            }
            // 残存する .bak を削除
            if entry.backup.exists() {
                let _ = std::fs::remove_file(&entry.backup);
            }
        }

        self.journal_file = None;
        let _ = std::fs::remove_file(&self.journal_path);
    }

    /// クラッシュなどで残存したジャーナルファイルを点検し、自動ロールバックとゴミ掃除を行う。
    pub fn recover<P: AsRef<Path>>(dir: P) -> Result<(), String> {
        let journal_path = dir.as_ref().join(JOURNAL_NAME);
        if !journal_path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&journal_path)
            .map_err(|e| format!("ジャーナルファイルを読み取れませんでした: {e}"))?;

        for line in content.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 3 {
                let target = Path::new(parts[0]);
                let backup = Path::new(parts[1]);
                let temp = Path::new(parts[2]);

                // バックアップが存在すれば、更新途中でクラッシュしたと判断して本番へ復元
                if backup.exists() {
                    let _ = std::fs::remove_file(target);
                    let _ = std::fs::rename(backup, target);
                }

                // 一時ファイルや残存バックアップを完全消去
                if temp.exists() {
                    let _ = std::fs::remove_file(temp);
                }
                if backup.exists() {
                    let _ = std::fs::remove_file(backup);
                }
            }
        }

        // 最後にジャーナル自身を削除
        let _ = std::fs::remove_file(&journal_path);
        Ok(())
    }
}

impl Drop for FileTransaction {
    fn drop(&mut self) {
        if !self.committed {
            self.rollback();
        }
    }
}
