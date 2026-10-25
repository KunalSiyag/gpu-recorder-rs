//! FFmpeg capability probing and the argument sets shared by the live capture
//! and the studio render pass.

use crate::settings::{AudioMode, Quality, Region};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoEncoder {
    Nvenc,
    Vaapi { device: PathBuf },
    X264,
}

impl VideoEncoder {
    pub fn label(&self) -> &'static str {
        match self {
            VideoEncoder::Nvenc => "NVENC",
            VideoEncoder::Vaapi { .. } => "VAAPI",
            VideoEncoder::X264 => "x264",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Capabilities {
    pub ffmpeg: bool,
    pub encoder: VideoEncoder,
    pub pulse: bool,
    /// `-fps_mode` exists since FFmpeg 5.1; older releases only know `-vsync`.
    pub fps_mode: bool,
}

impl Capabilities {
    /// Arguments that keep frame timestamps as they are.
    pub fn passthrough_timing(&self) -> [&'static str; 2] {
        if self.fps_mode {
            ["-fps_mode", "passthrough"]
        } else {
            ["-vsync", "passthrough"]
        }
    }
}

static CAPABILITIES: OnceLock<Capabilities> = OnceLock::new();

/// Probes FFmpeg once per process. Call `warm_up` at startup so the first
/// recording does not wait for the probe.
pub fn capabilities() -> &'static Capabilities {
    CAPABILITIES.get_or_init(probe_capabilities)
}

pub fn warm_up() {
    std::thread::spawn(|| {
        capabilities();
        crate::encoder::ActiveEncoderSession::warm_up();
    });
}

fn ffmpeg_ok(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn probe_capabilities() -> Capabilities {
    let devices = Command::new("ffmpeg")
        .args(["-hide_banner", "-devices"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(devices) = devices else {
        return Capabilities {
            ffmpeg: false,
            encoder: VideoEncoder::X264,
            pulse: false,
            fps_mode: true,
        };
    };
    let pulse = String::from_utf8_lossy(&devices.stdout)
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some("pulse") && l.trim_start().starts_with('D'));

    let test_src = ["-f", "lavfi", "-i", "color=c=black:s=256x256:r=1", "-frames:v", "1"];
    let encoder = if std::env::var("HYPERGSR_ENCODER").as_deref() == Ok("x264") {
        VideoEncoder::X264
    } else if ffmpeg_ok(&[&test_src[..], &["-vf", "format=bgr0", "-c:v", "h264_nvenc", "-f", "null", "-"]].concat()) {
        VideoEncoder::Nvenc
    } else {
        render_nodes()
            .into_iter()
            .find(|node| {
                let node = node.to_string_lossy();
                ffmpeg_ok(
                    &[
                        &["-vaapi_device", node.as_ref()][..],
                        &test_src[..],
                        &["-vf", "format=nv12,hwupload", "-c:v", "h264_vaapi", "-f", "null", "-"],
                    ]
                    .concat(),
                )
            })
            .map(|device| VideoEncoder::Vaapi { device })
            .unwrap_or(VideoEncoder::X264)
    };
    let fps_mode = ffmpeg_ok(&[&test_src[..], &["-fps_mode", "passthrough", "-f", "null", "-"]].concat());
    Capabilities {
        ffmpeg: true,
        encoder,
        pulse,
        fps_mode,
    }
}

fn render_nodes() -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .map(|dir| {
            dir.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("renderD"))
                })
                .collect()
        })
        .unwrap_or_default();
    nodes.sort();
    nodes
}

/// Describes what every encoder here produces: sRGB screen content in
/// limited-range BT.601 YCbCr. Without tags, players guess BT.709 for HD
/// video and shift the colors.
pub const COLOR_TAGS: &[&str] = &[
    "-colorspace",
    "smpte170m",
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-color_range",
    "tv",
];

fn push(args: &mut Vec<String>, items: &[&str]) {
    args.extend(items.iter().map(|s| s.to_string()));
}

/// Video encoder arguments for 4-byte RGB input (`bgr0`, `rgb0`, ...).
/// `filters` are applied before any upload; NVENC converts RGB to YUV on
/// the GPU, so it needs no conversion filter.
pub fn video_encoder_args(encoder: &VideoEncoder, quality: Quality, filters: &[String]) -> Vec<String> {
    let mut args = Vec::new();
    let mut chain: Vec<String> = filters.to_vec();
    match encoder {
        VideoEncoder::Nvenc => {
            if !chain.is_empty() {
                args.push("-vf".into());
                args.push(chain.join(","));
            }
            let cq = match quality {
                Quality::Standard => "24",
                Quality::High => "19",
            };
            push(
                &mut args,
                &["-c:v", "h264_nvenc", "-preset", "p5", "-tune", "hq", "-rc", "vbr", "-cq", cq, "-b:v", "0"],
            );
            push(&mut args, &["-spatial-aq", "1", "-profile:v", "high"]);
        }
        VideoEncoder::Vaapi { device } => {
            args.push("-vaapi_device".into());
            args.push(device.display().to_string());
            chain.push("format=nv12".into());
            chain.push("hwupload".into());
            args.push("-vf".into());
            args.push(chain.join(","));
            let qp = match quality {
                Quality::Standard => "25",
                Quality::High => "20",
            };
            push(&mut args, &["-c:v", "h264_vaapi", "-qp", qp]);
        }
        VideoEncoder::X264 => {
            if !chain.is_empty() {
                args.push("-vf".into());
                args.push(chain.join(","));
            }
            let crf = match quality {
                Quality::Standard => "23",
                Quality::High => "18",
            };
            push(
                &mut args,
                &["-c:v", "libx264", "-preset", "veryfast", "-crf", crf, "-pix_fmt", "yuv420p"],
            );
        }
    }
    args
}

/// Output arguments for the capture helper: audio inputs, encoder, and a
/// fragmented MP4 that stays playable up to the last keyframe if the recorder
/// is killed. The helper supplies the video input itself.
pub fn capture_output_args(
    encoder: &VideoEncoder,
    quality: Quality,
    fps: u32,
    audio: AudioMode,
    region: Option<Region>,
    output: &Path,
) -> Vec<String> {
    let mut args = Vec::new();
    let mut audio_inputs = 0;
    if audio.records_system() {
        push(&mut args, &["-f", "pulse", "-thread_queue_size", "1024", "-i", "@DEFAULT_MONITOR@"]);
        audio_inputs += 1;
    }
    if audio.records_microphone() {
        push(&mut args, &["-f", "pulse", "-thread_queue_size", "1024", "-i", "@DEFAULT_SOURCE@"]);
        audio_inputs += 1;
    }

    push(&mut args, &["-map", "0:v:0"]);
    match audio_inputs {
        0 => {}
        1 => push(&mut args, &["-map", "1:a:0"]),
        _ => push(
            &mut args,
            &[
                "-filter_complex",
                "[1:a][2:a]amix=inputs=2:duration=longest:normalize=0[mix]",
                "-map",
                "[mix]",
            ],
        ),
    }

    // 4:2:0 encoders need even sizes; a window can be any size. The fps
    // filter turns the wall-clock-stamped frames into a constant rate,
    // starting from the first frame's real time rather than zero.
    let filters = vec![
        region.map_or_else(|| "crop=trunc(iw/2)*2:trunc(ih/2)*2".to_string(), |r| r.crop_filter()),
        format!("fps={fps}"),
    ];
    args.extend(video_encoder_args(encoder, quality, &filters));
    push(&mut args, COLOR_TAGS);
    let gop = (fps * 2).to_string();
    push(&mut args, &capabilities().passthrough_timing());
    push(&mut args, &["-g", &gop]);
    if audio_inputs > 0 {
        push(&mut args, &["-c:a", "aac", "-b:a", "192k", "-shortest"]);
    }
    // Video and Pulse audio are both stamped with the wall clock; keeping
    // those stamps and shifting the file once keeps them in sync, where
    // FFmpeg would otherwise start each input at zero on its own.
    push(&mut args, &["-copyts", "-avoid_negative_ts", "make_zero"]);
    // delay_moov keeps each track's real start, so audio that began before
    // the first frame stays in sync instead of being pulled to zero.
    push(
        &mut args,
        &["-movflags", "+frag_keyframe+empty_moov+default_base_moof+delay_moov", "-f", "mp4"],
    );
    args.push(output.display().to_string());
    args
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    pub duration_sec: f64,
    pub has_audio: bool,
    /// Time of the first video frame; audio may start before it.
    pub video_start_sec: f64,
}

/// Reads size and duration from `ffmpeg -i`, which is always installed,
/// unlike ffprobe (the Flatpak ships without it).
pub fn probe_video(path: &Path) -> Result<VideoInfo> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-i"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .context("cannot run ffmpeg")?;
    let mut info = parse_probe(&String::from_utf8_lossy(&out.stderr))
        .with_context(|| format!("cannot read video info from {}", path.display()))?;
    info.video_start_sec = first_video_pts(path).unwrap_or(0.0);
    Ok(info)
}

/// Timestamp of the first decoded video frame, from FFmpeg's showinfo.
fn first_video_pts(path: &Path) -> Option<f64> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-frames:v", "1", "-vf", "showinfo", "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let rest = &text[text.find("pts_time:")? + "pts_time:".len()..];
    let value: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    value.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0 && *v < 5.0)
}

fn parse_probe(text: &str) -> Result<VideoInfo> {
    let mut duration_sec = 0.0;
    let mut size = None;
    let mut has_audio = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Duration: ") {
            let stamp = rest.split(',').next().unwrap_or("");
            let parts: Vec<f64> = stamp.split(':').filter_map(|p| p.trim().parse().ok()).collect();
            if parts.len() == 3 {
                duration_sec = parts[0] * 3600.0 + parts[1] * 60.0 + parts[2];
            }
        } else if line.starts_with("Stream #") && line.contains(": Video: ") && size.is_none() {
            size = line
                .split([',', ' '])
                .filter_map(|tok| {
                    let (w, h) = tok.split_once('x')?;
                    Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?))
                })
                .find(|&(w, h)| w > 0 && h > 0);
        } else if line.starts_with("Stream #") && line.contains(": Audio: ") {
            has_audio = true;
        }
    }
    let Some((width, height)) = size else {
        bail!("no video stream");
    };
    Ok(VideoInfo {
        width,
        height,
        duration_sec,
        has_audio,
        video_start_sec: 0.0,
    })
}

/// A capture file to join, and the file time it is worth keeping up to.
pub struct Segment {
    pub path: PathBuf,
    pub end_sec: Option<f64>,
}

fn concat_list(segments: &[Segment]) -> String {
    let mut list = String::from("ffconcat version 1.0\n");
    for seg in segments {
        let path = seg.path.display().to_string().replace('\'', "'\\''");
        list.push_str(&format!("file '{path}'\n"));
        if let Some(end) = seg.end_sec {
            list.push_str(&format!("outpoint {end:.6}\n"));
        }
    }
    list
}

/// Joins captures made with the same settings into one MP4 without
/// re-encoding. Each segment starts where the previous one's kept part ends.
pub fn concat_segments(segments: &[Segment], output: &Path) -> Result<()> {
    let list_path = output.with_extension("concat.txt");
    std::fs::write(&list_path, concat_list(segments)).context("cannot write the segment list")?;
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list_path)
        .args(["-map", "0", "-c", "copy", "-f", "mp4"])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .context("cannot run ffmpeg");
    let _ = std::fs::remove_file(&list_path);
    if !status?.success() {
        bail!("ffmpeg could not join the recording's parts");
    }
    Ok(())
}

/// Copies the streams into a web-friendly MP4 without re-encoding.
pub fn remux_to_mp4(input: &Path, output: &Path) -> Result<()> {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(input)
        .args(["-map", "0", "-c", "copy", "-movflags", "+faststart", "-f", "mp4"])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .context("cannot run ffmpeg")?;
    if !status.success() {
        bail!("ffmpeg could not remux {}", input.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffmpeg_input_banner() {
        let text = "Input #0, matroska,webm, from 'a.mkv':\n  Metadata:\n    ENCODER         : Lavf60.16.100\n  Duration: 00:01:05.85, start: 0.000000, bitrate: 1795 kb/s\n  Stream #0:0: Video: h264 (Main), yuv420p(tv, bt470bg/unknown/unknown, progressive), 2560x1600 [SAR 1:1 DAR 8:5], 60 fps, 60 tbr, 1k tbn\n  Stream #0:1: Audio: aac (LC), 48000 Hz, stereo, fltp\n";
        let info = parse_probe(text).unwrap();
        assert_eq!((info.width, info.height), (2560, 1600));
        assert!((info.duration_sec - 65.85).abs() < 1e-6);
        assert!(info.has_audio);
    }

    #[test]
    fn concat_list_quotes_paths_and_trims_segments() {
        let list = concat_list(&[
            Segment { path: PathBuf::from("/v/it's.part1.mp4"), end_sec: Some(64.9) },
            Segment { path: PathBuf::from("/v/b.mp4"), end_sec: None },
        ]);
        assert_eq!(
            list,
            "ffconcat version 1.0\nfile '/v/it'\\''s.part1.mp4'\noutpoint 64.900000\nfile '/v/b.mp4'\n"
        );
    }

    #[test]
    fn rejects_files_without_video() {
        assert!(parse_probe("  Duration: N/A\n  Stream #0:0: Audio: aac\n").is_err());
    }

    #[test]
    fn capture_args_mix_two_audio_sources_and_crop_regions() {
        let region = Region { x: 0.1, y: 0.2, w: 0.5, h: 0.5 };
        let args = capture_output_args(
            &VideoEncoder::Nvenc,
            Quality::High,
            60,
            AudioMode::Both,
            Some(region),
            Path::new("/tmp/out.capture.mp4"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("@DEFAULT_MONITOR@"));
        assert!(joined.contains("@DEFAULT_SOURCE@"));
        assert!(joined.contains("amix=inputs=2"));
        assert!(joined.contains("-shortest"));
        assert!(joined.contains("crop=w=trunc(iw*0.50000/2)*2"));
        let plain = capture_output_args(&VideoEncoder::X264, Quality::High, 30, AudioMode::Off, None, Path::new("o.mp4"));
        assert!(plain.join(" ").contains("crop=trunc(iw/2)*2:trunc(ih/2)*2"));
        assert!(!plain.join(" ").contains("-shortest"));
        assert_eq!(args.last().unwrap(), "/tmp/out.capture.mp4");
    }
}
