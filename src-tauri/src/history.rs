use std::fs;
use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavSpec, WavWriter};

use crate::capture::CapturedAudio;
use crate::error::AppResult;

const MAX_ENTRIES: usize = 5;

pub struct HistoryStore {
    dir: PathBuf,
}

pub struct HistoryEntry {
    pub timestamp: String,
    pub preview: String,
    pub has_audio: bool,
}

impl HistoryStore {
    pub fn new(config_dir: &Path) -> AppResult<Self> {
        let dir = config_dir.join("history");
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Persist audio to disk immediately, before transcription.
    /// Returns the path to the written WAV file.
    pub fn persist_audio(&self, audio: &CapturedAudio) -> AppResult<PathBuf> {
        let ts = chrono_timestamp();
        self.write_wav(&ts, audio)?;
        Ok(self.dir.join(format!("entry-{ts}.wav")))
    }

    /// Save a history entry (text + optional audio). Prunes to 5 entries afterward.
    ///
    /// If `prewritten_wav` is provided, the text file is written with the same
    /// timestamp stem so it pairs with the already-persisted WAV. The WAV is
    /// not re-written.
    pub fn save_entry(
        &self,
        raw_transcript: &str,
        final_text: Option<&str>,
        model_name: Option<&str>,
        audio: Option<&CapturedAudio>,
        prewritten_wav: Option<&Path>,
    ) -> AppResult<()> {
        if let Some(wav_path) = prewritten_wav {
            // Extract timestamp from the pre-written WAV filename
            let stem = wav_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let ts = stem.strip_prefix("entry-").unwrap_or(stem);
            self.write_text(ts, raw_transcript, final_text, model_name)?;
        } else {
            let ts = chrono_timestamp();
            self.write_text(&ts, raw_transcript, final_text, model_name)?;
            if let Some(audio) = audio {
                self.write_wav(&ts, audio)?;
            }
        }
        self.prune()?;
        Ok(())
    }

    /// List all history entries, newest first.
    pub fn list_entries(&self) -> AppResult<Vec<HistoryEntry>> {
        let mut entries = Vec::new();
        let dir = match fs::read_dir(&self.dir) {
            Ok(d) => d,
            Err(_) => return Ok(entries),
        };

        let mut txt_files: Vec<PathBuf> = dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "txt"))
            .collect();

        txt_files.sort();
        txt_files.reverse();

        for path in txt_files {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let timestamp = stem.strip_prefix("entry-").unwrap_or(stem).to_string();
            let has_audio = path.with_extension("wav").exists();
            let preview = fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .find(|l| !l.starts_with("---") && !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(120)
                .collect();

            entries.push(HistoryEntry {
                timestamp,
                preview,
                has_audio,
            });
        }

        Ok(entries)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn write_text(
        &self,
        ts: &str,
        raw_transcript: &str,
        final_text: Option<&str>,
        model_name: Option<&str>,
    ) -> AppResult<()> {
        let path = self.dir.join(format!("entry-{ts}.txt"));
        let mut content = String::new();
        content.push_str("--- Raw Transcript ---\n");
        content.push_str(raw_transcript);
        content.push('\n');

        if let Some(rewrite) = final_text {
            let label = model_name.unwrap_or("gemini");
            content.push_str(&format!("\n--- Rewritten ({label}) ---\n"));
            content.push_str(rewrite);
            content.push('\n');
        }

        fs::write(&path, content)?;
        Ok(())
    }

    fn write_wav(&self, ts: &str, audio: &CapturedAudio) -> AppResult<()> {
        let path = self.dir.join(format!("entry-{ts}.wav"));
        let spec = WavSpec {
            channels: audio.channels,
            sample_rate: audio.sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut writer = WavWriter::create(&path, spec)?;
        for &sample in &audio.samples {
            let clamped = sample.clamp(-1.0, 1.0);
            let val = (clamped * i16::MAX as f32) as i16;
            writer.write_sample(val)?;
        }
        writer.finalize()?;
        Ok(())
    }

    fn prune(&self) -> AppResult<()> {
        let mut txt_files: Vec<PathBuf> = fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "txt"))
            .collect();

        if txt_files.len() <= MAX_ENTRIES {
            return Ok(());
        }

        txt_files.sort();
        let to_remove = txt_files.len() - MAX_ENTRIES;
        for path in &txt_files[..to_remove] {
            let _ = fs::remove_file(path);
            let wav = path.with_extension("wav");
            let _ = fs::remove_file(wav);
        }
        Ok(())
    }
}

fn chrono_timestamp() -> String {
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:020}", dur.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, HistoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = HistoryStore::new(tmp.path()).unwrap();
        (tmp, store)
    }

    fn dummy_audio() -> CapturedAudio {
        CapturedAudio {
            samples: vec![0.0, 0.5, -0.5, 1.0, -1.0],
            sample_rate: 16000,
            channels: 1,
        }
    }

    #[test]
    fn save_and_list_entries() {
        let (_tmp, store) = make_store();
        store
            .save_entry("hello world", None, None, None, None)
            .unwrap();
        let entries = store.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].preview.contains("hello world"));
        assert!(!entries[0].has_audio);
    }

    #[test]
    fn transcript_only_entry_does_not_write_audio() {
        let (_tmp, store) = make_store();

        store
            .save_entry("imported transcript", None, None, None, None)
            .unwrap();

        let files: Vec<_> = store
            .dir()
            .read_dir()
            .unwrap()
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0]
                .path()
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("txt")
        );
    }

    #[test]
    fn save_with_audio_and_gemini() {
        let (_tmp, store) = make_store();
        let audio = dummy_audio();
        store
            .save_entry(
                "raw text",
                Some("rewritten text"),
                Some("gemini-2.5-flash"),
                Some(&audio),
                None,
            )
            .unwrap();
        let entries = store.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].has_audio);

        // Verify text file contents
        let txt_path = store
            .dir()
            .read_dir()
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().is_some_and(|ext| ext == "txt"))
            .unwrap()
            .path();
        let content = fs::read_to_string(txt_path).unwrap();
        assert!(content.contains("--- Raw Transcript ---"));
        assert!(content.contains("raw text"));
        assert!(content.contains("--- Rewritten (gemini-2.5-flash) ---"));
        assert!(content.contains("rewritten text"));
    }

    #[test]
    fn prune_keeps_only_five() {
        let (_tmp, store) = make_store();
        for i in 0..7 {
            store
                .save_entry(&format!("entry {i}"), None, None, None, None)
                .unwrap();
            // Small delay so timestamps differ
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let entries = store.list_entries().unwrap();
        assert_eq!(entries.len(), 5);
        // Newest entry should be "entry 6"
        assert!(entries[0].preview.contains("entry 6"));
    }

    #[test]
    fn persist_audio_then_save_entry_reuses_wav_timestamp() {
        let (_tmp, store) = make_store();
        let audio = dummy_audio();
        let wav_path = store.persist_audio(&audio).unwrap();

        store
            .save_entry("raw text", None, None, None, Some(&wav_path))
            .unwrap();

        let txt_path = wav_path.with_extension("txt");
        assert!(wav_path.exists());
        assert!(txt_path.exists());
        assert_eq!(
            fs::read_to_string(txt_path).unwrap().contains("raw text"),
            true
        );
        assert_eq!(
            store
                .dir()
                .read_dir()
                .unwrap()
                .filter_map(|entry| entry.ok())
                .count(),
            2
        );
    }
}
