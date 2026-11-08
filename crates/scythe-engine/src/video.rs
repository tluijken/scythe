//! REAPER's video sink, written with the `ffmpeg` command-line tool: the rendered audio muxed
//! with black frames and the project's video items.

use std::path::{Path, PathBuf};
use std::process::Command;

use scythe_core::render_settings::{AudioCodec, Container, Video, VideoCodec};
use scythe_core::{Project, Source, TimeRange};

const VIDEO_EXTENSIONS: &[&str] =
    &["mp4", "m4v", "mov", "qt", "mkv", "webm", "ogv", "avi", "mpg", "mpeg", "wmv", "flv", "mxf", "gif"];

/// A video item as it appears in the render.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoClip {
    pub path: PathBuf,
    /// Start in the render, in seconds from its start.
    pub start: f64,
    pub length: f64,
    /// Where the clip starts in the file, and how fast it plays.
    pub offset: f64,
    pub rate: f64,
}

/// The video items of unmuted tracks that reach into `range`, bottom track first so that the
/// top track, which REAPER shows, is drawn last.
pub fn video_clips(project: &Project, range: TimeRange) -> Vec<VideoClip> {
    let mut clips = Vec::new();
    for track in project.tracks.iter().rev().filter(|t| !t.muted) {
        for item in track.items.iter().filter(|i| !i.muted) {
            let Some(take) = item.take() else { continue };
            let Some((file, section_start)) = video_file(&take.source) else { continue };
            let (start, end) = (item.position.max(range.start), item.end().min(range.end));
            if end <= start {
                continue;
            }
            let rate = if take.playrate > 0.0 { take.playrate } else { 1.0 };
            clips.push(VideoClip {
                path: scythe_core::rpp::resolve_media_path(project, &file),
                start: start - range.start,
                length: end - start,
                offset: section_start + take.source_offset + (start - item.position) * rate,
                rate,
            });
        }
    }
    clips
}

/// The video file a take plays and where its section starts: REAPER's `VIDEO` sources, or a
/// file source with a video extension.
fn video_file(source: &Source) -> Option<(PathBuf, f64)> {
    match source {
        Source::File(path) => {
            let ext = path.extension().and_then(|e| e.to_str())?;
            VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(ext)).then(|| (path.clone(), 0.0))
        }
        Source::Raw(chunk) if chunk.args().first().is_some_and(|k| k == "VIDEO") => {
            Some((PathBuf::from(chunk.find_line("FILE")?.into_iter().next()?), 0.0))
        }
        Source::Section(s) => video_file(&s.inner).map(|(path, start)| (path, start + s.start)),
        _ => None,
    }
}

/// The frame rate as a fraction; NTSC rates (29.97 …) become their exact /1001 values.
fn frame_rate(fps: f64) -> (u64, u64) {
    let ntsc = (fps * 1.001).round();
    if (fps - ntsc * 1000.0 / 1001.0).abs() < 0.005 && (fps - fps.round()).abs() > 0.005 {
        return (ntsc as u64 * 1000, 1001);
    }
    if (fps - fps.round()).abs() < 1e-6 {
        return (fps.round() as u64, 1);
    }
    ((fps * 1000.0).round() as u64, 1000)
}

fn video_args(codec: VideoCodec, container: Container) -> Vec<&'static str> {
    match codec {
        VideoCodec::H264 => vec!["-c:v", "libx264", "-pix_fmt", "yuv420p"],
        VideoCodec::H265 => vec!["-c:v", "libx265", "-pix_fmt", "yuv420p"],
        VideoCodec::Av1 => vec!["-c:v", "libsvtav1", "-pix_fmt", "yuv420p"],
        VideoCodec::Vp8 => vec!["-c:v", "libvpx", "-pix_fmt", "yuv420p"],
        VideoCodec::Vp9 => vec!["-c:v", "libvpx-vp9", "-pix_fmt", "yuv420p"],
        VideoCodec::Mpeg1 => vec!["-c:v", "mpeg1video", "-pix_fmt", "yuv420p"],
        VideoCodec::Mpeg2 => vec!["-c:v", "mpeg2video", "-pix_fmt", "yuv420p"],
        VideoCodec::Xvid if container == Container::Avi => {
            vec!["-c:v", "mpeg4", "-vtag", "xvid", "-pix_fmt", "yuv420p"]
        }
        VideoCodec::Xvid => vec!["-c:v", "mpeg4", "-pix_fmt", "yuv420p"],
        VideoCodec::Flv1 => vec!["-c:v", "flv", "-pix_fmt", "yuv420p"],
        VideoCodec::Ffv1 => vec!["-c:v", "ffv1", "-pix_fmt", "bgra"],
        VideoCodec::Huffyuv => vec!["-c:v", "huffyuv", "-pix_fmt", "bgra"],
        VideoCodec::Mjpeg => vec!["-c:v", "mjpeg", "-pix_fmt", "yuvj420p"],
        VideoCodec::Dv => vec!["-c:v", "dvvideo"],
    }
}

fn audio_encoder(codec: AudioCodec) -> Option<&'static str> {
    Some(match codec {
        AudioCodec::Mp3 => "libmp3lame",
        AudioCodec::Aac => "aac",
        AudioCodec::Ac3 => "ac3",
        AudioCodec::Mp2 => "mp2",
        AudioCodec::Vorbis => "libvorbis",
        AudioCodec::Opus => "libopus",
        AudioCodec::Pcm16 => "pcm_s16le",
        AudioCodec::Pcm24 => "pcm_s24le",
        AudioCodec::Float32 => "pcm_f32le",
        // FFmpeg has no DV audio encoder.
        AudioCodec::DvAudio => return None,
    })
}

fn muxer(container: Container) -> &'static str {
    match container {
        Container::Avi => "avi",
        Container::Mpeg1 => "mpeg",
        Container::Mpeg2 => "mpegts",
        Container::QuickTimeLegacy | Container::Mov => "mov",
        Container::Matroska => "matroska",
        Container::Flv => "flv",
        Container::WebM => "webm",
        Container::Mp4 => "mp4",
    }
}

/// The `ffmpeg` arguments that write `out` from the rendered `audio` (when the sink has an
/// audio stream) and `clips` over black frames of `duration` seconds (when it has a video
/// stream). A frame shows a clip when the clip covers any part of it, as in REAPER's renders.
pub fn ffmpeg_args(
    video: &Video,
    clips: &[VideoClip],
    duration: f64,
    audio: Option<&Path>,
    out: &Path,
) -> Result<Vec<String>, String> {
    if video.video.is_none() && video.audio.is_none() {
        return Err("audio and video are both disabled".into());
    }
    let mut args: Vec<String> = ["-y", "-nostdin", "-v", "error"].map(String::from).to_vec();
    let mut inputs = 0;
    let mut maps = Vec::new();
    if let Some(codec) = video.video {
        let (num, den) = frame_rate(video.fps);
        let fps = num as f64 / den as f64;
        let (w, h) = (video.width, video.height);
        args.extend(["-f".into(), "lavfi".into(), "-i".into()]);
        args.push(format!("color=c=black:s={w}x{h}:r={num}/{den}:d={duration}"));
        inputs += 1;
        let mut graph = String::from("[0:v]format=rgba[base0]");
        for (n, clip) in clips.iter().enumerate() {
            let first = (clip.start * fps + 1e-9).floor() / fps;
            let end = ((clip.start + clip.length) * fps - 1e-9).ceil() / fps;
            args.extend(["-ss".into(), format!("{}", clip.offset.max(0.0)), "-t".into()]);
            args.push(format!("{}", clip.length * clip.rate));
            args.extend(["-i".into(), clip.path.to_string_lossy().into_owned()]);
            // REAPER's own scaler is not matched; bilinear comes closest of FFmpeg's on average.
            let fit = if video.keep_aspect {
                format!(
                    "scale={w}:{h}:flags=bilinear:force_original_aspect_ratio=decrease,\
                     pad={w}:{h}:-1:-1:color=black"
                )
            } else {
                format!("scale={w}:{h}:flags=bilinear")
            };
            graph.push_str(&format!(
                ";[{inputs}:v]{fit},setsar=1,setpts=(PTS-STARTPTS)/{rate},fps={num}/{den},format=rgba,\
                 tpad=start_duration={first}:color=black@0.0[clip{n}];\
                 [base{n}][clip{n}]overlay=eof_action=repeat:enable='lt(t,{end})'[base{next}]",
                rate = clip.rate,
                next = n + 1,
            ));
            inputs += 1;
        }
        args.extend(["-filter_complex".into(), graph]);
        maps.extend(["-map".into(), format!("[base{}]", clips.len())]);
        maps.extend(video_args(codec, video.container).into_iter().map(String::from));
        if !matches!(codec, VideoCodec::Ffv1 | VideoCodec::Huffyuv | VideoCodec::Dv) {
            maps.extend(["-b:v".into(), format!("{}k", video.video_kbps.max(1))]);
        }
        maps.extend(["-r".into(), format!("{num}/{den}")]);
    }
    if let Some(codec) = video.audio {
        let encoder = audio_encoder(codec).ok_or("the DV audio codec is not supported")?;
        let wav = audio.ok_or("the render has no audio")?;
        args.extend(["-i".into(), wav.to_string_lossy().into_owned()]);
        maps.extend(["-map".into(), format!("{inputs}:a"), "-c:a".into(), encoder.into()]);
        if !matches!(codec, AudioCodec::Pcm16 | AudioCodec::Pcm24 | AudioCodec::Float32) {
            maps.extend(["-b:a".into(), format!("{}k", video.audio_kbps.max(1))]);
        }
    }
    args.extend(maps);
    args.extend(["-t".into(), format!("{duration}"), "-f".into(), muxer(video.container).into()]);
    args.push(out.to_string_lossy().into_owned());
    Ok(args)
}

/// Runs `ffmpeg` with [`ffmpeg_args`].
pub fn write_video(
    video: &Video,
    clips: &[VideoClip],
    duration: f64,
    audio: Option<&Path>,
    out: &Path,
) -> Result<(), String> {
    let args = ffmpeg_args(video, clips, duration, audio, out)?;
    let output =
        Command::new("ffmpeg").args(&args).output().map_err(|e| format!("video renders need `ffmpeg` on PATH: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rates() {
        assert_eq!(frame_rate(30.0), (30, 1));
        assert_eq!(frame_rate(29.97), (30000, 1001));
        assert_eq!(frame_rate(23.976), (24000, 1001));
        assert_eq!(frame_rate(12.5), (12500, 1000));
    }

    #[test]
    fn ffmpeg_arguments() {
        let mut v = Video {
            container: Container::Matroska,
            video: None,
            audio: Some(AudioCodec::Pcm16),
            video_kbps: 2048,
            audio_kbps: 128,
            width: 320,
            height: 240,
            fps: 25.0,
            keep_aspect: true,
        };
        let args = ffmpeg_args(&v, &[], 2.0, Some(Path::new("a.wav")), Path::new("o.mkv")).unwrap().join(" ");
        assert_eq!(args, "-y -nostdin -v error -i a.wav -map 0:a -c:a pcm_s16le -t 2 -f matroska o.mkv");
        v.video = Some(VideoCodec::H264);
        v.audio = Some(AudioCodec::Aac);
        let args = ffmpeg_args(&v, &[], 2.0, Some(Path::new("a.wav")), Path::new("o.mkv")).unwrap().join(" ");
        assert!(args.contains("color=c=black:s=320x240:r=25/1:d=2"), "{args}");
        assert!(args.contains("-map 1:a -c:a aac -b:a 128k"), "{args}");
        assert!(args.contains("-c:v libx264 -pix_fmt yuv420p -b:v 2048k -r 25/1"), "{args}");
        v.video = None;
        v.audio = None;
        assert!(ffmpeg_args(&v, &[], 2.0, None, Path::new("o.mkv")).is_err());
    }

    #[test]
    fn clips_come_from_video_items_of_unmuted_tracks() {
        use scythe_core::{Item, Take};
        let mut p = Project::default();
        let top = p.add_track("top");
        let bottom = p.add_track("bottom");
        let muted = p.add_track("muted");
        let mut take = Take::from_file("/m/a.mp4".into());
        take.source_offset = 1.0;
        p.track_mut(top).unwrap().items.push(Item::new(0.5, 2.0, take));
        p.track_mut(bottom).unwrap().items.push(Item::new(0.0, 4.0, Take::from_file("/m/b.MOV".into())));
        p.track_mut(bottom).unwrap().items.push(Item::new(0.0, 4.0, Take::from_file("/m/c.wav".into())));
        p.track_mut(muted).unwrap().items.push(Item::new(0.0, 4.0, Take::from_file("/m/d.mkv".into())));
        p.track_mut(muted).unwrap().muted = true;
        let clips = video_clips(&p, TimeRange::new(1.0, 3.0));
        let names: Vec<_> = clips.iter().map(|c| c.path.file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["b.MOV", "a.mp4"]);
        assert_eq!((clips[1].start, clips[1].length, clips[1].offset), (0.0, 1.5, 1.5));
    }
}
