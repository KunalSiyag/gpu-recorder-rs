//! Instant replay: the screen is captured continuously into a ring of short
//! MPEG-TS segments on disk. Saving joins the newest segments into an MP4 of
//! the last 30 seconds or 2 minutes while the capture goes on. Clips are
//! saved as captured (no zoom or background), which keeps saving instant;
//! the editor can frame them afterwards.

use crate::encoder::{ActiveEncoderSession, CaptureOptions};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// Length of one segment; matches the capture's two-second keyframe interval.
const SEGMENT_SEC: u32 = 2;

pub struct ReplayBuffer {
    pub session: ActiveEncoderSession,
    dir: PathBuf,
    /// How much of the past a clip keeps.
    pub keep_sec: u32,
}

impl ReplayBuffer {
    pub fn start(options: &CaptureOptions, keep_sec: u32) -> Result<Self> {
        let root = crate::bundled::cache_dir().join("replay");
        remove_abandoned(&root);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let dir = root.join(format!("{stamp}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).context("cannot create the replay buffer")?;
        // Room for the clip, the segment being written, and one being read.
        let count = keep_sec.div_ceil(SEGMENT_SEC) + 3;
        match ActiveEncoderSession::start_ring(&dir, SEGMENT_SEC, count, options) {
            Ok(session) => Ok(Self { session, dir, keep_sec }),
            Err(err) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(err)
            }
        }
    }

    /// The segments a clip saved now needs, ready to join on another thread.
    pub fn clip(&self, videos_dir: &Path) -> Result<ReplayClip> {
        let segments = newest_segments(&self.dir, self.keep_sec.div_ceil(SEGMENT_SEC) as usize + 1);
        if segments.is_empty() {
            bail!("nothing is buffered yet");
        }
        let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let output = (1..)
            .map(|n| {
                let suffix = if n == 1 { String::new() } else { format!("-{n}") };
                videos_dir.join(format!("HyperGSR_Replay_{stamp}{suffix}.mp4"))
            })
            .find(|p| !p.exists())
            .expect("an unused file name");
        Ok(ReplayClip { segments, output, keep_sec: self.keep_sec })
    }

    /// Stops capturing and deletes the buffer. Blocks while the capture
    /// winds down, so call it off the UI thread.
    pub fn stop(self) {
        let _ = self.session.finish_capture();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Deletes buffers left by recorders that are no longer running (each
/// buffer's name ends in its recorder's pid).
fn remove_abandoned(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let pid = name.rsplit('-').next().and_then(|p| p.parse::<i32>().ok());
        let alive = pid.is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0);
        if !alive {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The `n` most recently written segments in `dir`, oldest first.
fn newest_segments(dir: &Path, n: usize) -> Vec<PathBuf> {
    let mut segments: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "ts"))
                .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0))
                .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
                .collect()
        })
        .unwrap_or_default();
    segments.sort();
    let skip = segments.len().saturating_sub(n);
    segments.into_iter().skip(skip).map(|(_, p)| p).collect()
}

/// A clip to save from a snapshot of the buffer.
pub struct ReplayClip {
    segments: Vec<PathBuf>,
    pub output: PathBuf,
    keep_sec: u32,
}

impl ReplayClip {
    /// Joins the segments (MPEG-TS concatenates byte for byte) and keeps the
    /// last `keep_sec` seconds, from the keyframe before, without
    /// re-encoding.
    pub fn save(self) -> Result<PathBuf> {
        let joined = self.output.with_file_name(format!(
            ".{}.joining.mp4",
            self.output.file_stem().and_then(|s| s.to_str()).unwrap_or("replay")
        ));
        let input = format!(
            "concat:{}",
            self.segments.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("|")
        );
        let run = |args: &[&str], out: &Path| -> Result<()> {
            let status = Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(args)
                .arg(out)
                .stdin(Stdio::null())
                .status()
                .context("cannot run ffmpeg")?;
            if !status.success() {
                bail!("ffmpeg could not save the clip ({status})");
            }
            Ok(())
        };
        let joined_ok = run(&["-i", &input, "-map", "0", "-c", "copy", "-f", "mp4"], &joined);
        let from_end = format!("-{}", self.keep_sec);
        let joined_name = joined.display().to_string();
        let result = joined_ok.and_then(|_| {
            run(
                &["-sseof", &from_end, "-i", &joined_name, "-map", "0", "-c", "copy", "-movflags", "+faststart", "-f", "mp4"],
                &self.output,
            )
        });
        let _ = std::fs::remove_file(&joined);
        if result.is_err() {
            let _ = std::fs::remove_file(&self.output);
        }
        result.map(|_| self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_segments_across_the_wrap() {
        let dir = std::env::temp_dir().join(format!("hypergsr-replay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Written in the order 3, 4, 0, 1 (the ring wrapped); 2 is empty.
        for name in ["seg003.ts", "seg004.ts", "seg000.ts", "seg001.ts"] {
            std::fs::write(dir.join(name), b"x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        std::fs::write(dir.join("seg002.ts"), b"").unwrap();
        let names: Vec<String> = newest_segments(&dir, 3)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["seg004.ts", "seg000.ts", "seg001.ts"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
