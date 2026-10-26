//! Recent recordings, kept re-renderable: the raw capture next to the
//! render job that made the saved video (telemetry, cursor track, cuts,
//! effects). The editor re-renders from it with a trim, another shape, or
//! another format. The oldest projects are deleted to stay within a few
//! recordings and a disk budget.

use crate::studio_render::RenderJob;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many recordings stay editable, and how much disk they may use.
const KEEP: usize = 5;
const MAX_BYTES: u64 = 6 << 30;

/// A kept recording.
#[derive(Clone, Serialize, Deserialize)]
pub struct Project {
    /// The render that made the saved video, reading the kept capture.
    pub job: RenderJob,
    /// Whether the telemetry can steer a zoom (a window recorded without
    /// its own cursor track cannot).
    pub zoomable: bool,
}

const CAPTURE: &str = "capture.mp4";
const WEBCAM: &str = "webcam.mkv";
const JOB: &str = "project.json";

fn root() -> PathBuf {
    crate::bundled::cache_dir().join("projects")
}

/// Whether recordings are kept for the editor (`HYPERGSR_KEEP_PROJECTS=0`
/// turns it off).
pub fn enabled() -> bool {
    std::env::var("HYPERGSR_KEEP_PROJECTS").as_deref() != Ok("0")
}

fn dir_for(output: &Path) -> Option<PathBuf> {
    let stem = output.file_stem()?.to_string_lossy().into_owned();
    Some(root().join(stem))
}

/// Keeps `job.capture` as the project of `job.output`, moving the file.
/// The capture is deleted when it cannot be kept.
pub fn keep(job: &RenderJob, zoomable: bool) {
    if !enabled() || keep_in(&root(), job, zoomable).is_err() {
        let _ = std::fs::remove_file(&job.capture);
        if let Some(cam) = &job.webcam {
            let _ = std::fs::remove_file(&cam.path);
        }
    }
    prune(&root(), KEEP, MAX_BYTES);
}

fn keep_in(root: &Path, job: &RenderJob, zoomable: bool) -> Result<()> {
    let stem = job.output.file_stem().context("no file name")?;
    let dir = root.join(stem);
    std::fs::create_dir_all(&dir)?;
    // Only a rename: copying a long capture across disks is not worth it.
    std::fs::rename(&job.capture, dir.join(CAPTURE))?;
    let mut saved = job.clone();
    saved.capture = PathBuf::from(CAPTURE);
    if let Some(cam) = &mut saved.webcam {
        if std::fs::rename(&cam.path, dir.join(WEBCAM)).is_ok() {
            cam.path = PathBuf::from(WEBCAM);
        } else {
            let _ = std::fs::remove_file(&cam.path);
            saved.webcam = None;
        }
    }
    // JSON has no infinity; a cut to the end becomes one far past it.
    for cut in &mut saved.cuts {
        cut.1 = cut.1.min(1e9);
    }
    for span in saved.webcam.iter_mut().flat_map(|c| c.clock.iter_mut()) {
        span.last = span.last.min(1e9);
    }
    std::fs::write(dir.join(JOB), serde_json::to_vec(&Project { job: saved, zoomable })?)?;
    Ok(())
}

/// How `output` was made, pointing at its kept capture.
pub fn find(output: &Path) -> Option<Project> {
    find_in(&dir_for(output)?, output)
}

fn find_in(dir: &Path, output: &Path) -> Option<Project> {
    let mut project: Project = serde_json::from_slice(&std::fs::read(dir.join(JOB)).ok()?).ok()?;
    let capture = dir.join(CAPTURE);
    if !capture.is_file() || project.job.output.file_name() != output.file_name() {
        return None;
    }
    project.job.capture = capture;
    if let Some(cam) = &mut project.job.webcam {
        cam.path = dir.join(WEBCAM);
    }
    if project.job.webcam.as_ref().is_some_and(|c| !c.path.is_file()) {
        project.job.webcam = None;
    }
    Some(project)
}

/// Deletes the oldest projects beyond `keep` or `max_bytes`.
fn prune(root: &Path, keep: usize, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let mut projects: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let path = e.path();
            let size = [CAPTURE, WEBCAM]
                .iter()
                .map(|f| std::fs::metadata(path.join(f)).map_or(0, |m| m.len()))
                .sum();
            let time = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            (time, size, path)
        })
        .collect();
    projects.sort_by(|a, b| b.0.cmp(&a.0));
    let mut used = 0u64;
    for (i, (_, size, path)) in projects.iter().enumerate() {
        used += size;
        if i >= keep || (i > 0 && used > max_bytes) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Aspect, BackgroundSettings, ExportFormat, Quality};

    fn job(capture: PathBuf, output: &str) -> RenderJob {
        RenderJob {
            capture,
            output: PathBuf::from(output),
            events: Vec::new(),
            auto_zoom: true,
            region: None,
            background: BackgroundSettings::default(),
            quality: Quality::High,
            fps: 60,
            cursor: None,
            cursor_scale: 1.0,
            click_ripples: true,
            cuts: vec![(0.0, 3.0), (9.0, f64::INFINITY)],
            aspect: Aspect::Original,
            format: ExportFormat::Mp4,
            webcam: None,
        }
    }

    #[test]
    fn kept_projects_round_trip_and_old_ones_are_pruned() {
        let base = std::env::temp_dir().join(format!("hypergsr-projects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        for n in 0..3 {
            let capture = base.join(format!("c{n}.capture.mp4"));
            std::fs::write(&capture, vec![0u8; 100]).unwrap();
            keep_in(&base.join("p"), &job(capture.clone(), &format!("/v/HyperGSR_{n}.mp4")), true).unwrap();
            assert!(!capture.exists(), "moved");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let found = find_in(&base.join("p/HyperGSR_2"), Path::new("/v/HyperGSR_2.mp4")).expect("found").job;
        assert_eq!(found.capture, base.join("p/HyperGSR_2").join(CAPTURE));
        assert_eq!(found.cuts[1].0, 9.0);
        assert!(found.cuts[1].1 >= 1e9);
        assert!(find_in(&base.join("p/HyperGSR_2"), Path::new("/v/other.mp4")).is_none());

        prune(&base.join("p"), 2, u64::MAX);
        assert!(!base.join("p/HyperGSR_0").exists());
        assert!(base.join("p/HyperGSR_2").exists());
        // Over budget, only the newest stays.
        prune(&base.join("p"), 5, 150);
        assert!(!base.join("p/HyperGSR_1").exists());
        assert!(base.join("p/HyperGSR_2").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
