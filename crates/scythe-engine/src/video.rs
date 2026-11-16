//! REAPER's video sink, written with the `ffmpeg` command-line tool: the rendered audio muxed
//! with black frames and the project's video items.

use std::path::{Path, PathBuf};

use scythe_core::render_settings::{AudioCodec, Container, Video, VideoCodec};
use scythe_core::rpp::{Chunk, Node, line_key};
use scythe_core::{Project, Source, TimeRange};

const VIDEO_EXTENSIONS: &[&str] =
    &["mp4", "m4v", "mov", "qt", "mkv", "webm", "ogv", "avi", "mpg", "mpeg", "wmv", "flv", "mxf", "gif"];

/// Whether `path` has the extension of a video file.
pub fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(e)))
}

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
    /// The file's frame size, when known; REAPER's scaler is reproduced only then.
    pub size: Option<(u32, u32)>,
    /// The take's video processors, then its track's.
    pub effects: Vec<VideoEffect>,
}

impl VideoClip {
    fn placement(&self) -> Option<&OpacityZoomPan> {
        self.effects.iter().rev().find_map(|e| match e {
            VideoEffect::OpacityZoomPan(p) => Some(p),
            _ => None,
        })
    }
}

/// A stock preset of REAPER's video processor, recognised by its code (which projects store
/// in full), with its parameters. Other processors run EEL code with REAPER's drawing API and
/// are left out of renders.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoEffect {
    /// "Basic helpers: Track opacity/zoom/pan" (without rotation or a forced size).
    OpacityZoomPan(OpacityZoomPan),
    /// "FX: Invert colors".
    Invert,
    /// "Basic helpers: Brightness/Contrast": the luma and chroma tables it computes.
    BrightnessContrast { brightness: f64, brightness_pre: f64, contrast: f64, saturation: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpacityZoomPan {
    pub opacity: f64,
    /// In tenths of a decade: the frame is scaled by 10^(zoom/10).
    pub zoom: f64,
    /// -1 to 1: from just off the left (top) edge to just off the right (bottom).
    pub x_offset: f64,
    pub y_offset: f64,
    /// Bilinear filtering; without it the video is scaled by nearest neighbour.
    pub filter: bool,
    /// Black behind an opaque video; otherwise the tracks below show around it.
    pub clear: bool,
    pub keep_aspect: bool,
}

impl OpacityZoomPan {
    /// The processor snaps opacity below 0.01 to 0 and above 0.99 to 1.
    fn alpha(&self) -> f64 {
        match self.opacity {
            o if o < 0.01 => 0.0,
            o if o > 0.99 => 1.0,
            o => o,
        }
    }

    fn hides_background(&self) -> bool {
        self.alpha() >= 1.0 && self.clear
    }

    /// The filters that scale a source of `size` into the processor's rectangle, and where
    /// that rectangle sits in the frame; `None` when nothing is drawn.
    fn placement(&self, size: Option<(u32, u32)>, (w, h): (u32, u32)) -> Option<(String, i64, i64)> {
        if self.alpha() <= 0.0 {
            return None;
        }
        let z = 10f64.powf(self.zoom / 10.0);
        let (dw, dh) = ((w as f64 * z) as i64 & !1, (h as f64 * z) as i64 & !1);
        if dw <= 0 || dh <= 0 {
            return None;
        }
        // EEL's `|0` truncates towards zero.
        let x = ((w as i64 - dw) as f64 + self.x_offset * (w as i64 + dw) as f64) * 0.5;
        let y = ((h as i64 - dh) as f64 + self.y_offset * (h as i64 + dh) as f64) * 0.5;
        let rect = (dw as u32, dh as u32);
        let filter = match size {
            Some(size) => {
                let (fw, fh, ox, oy) = if self.keep_aspect { fit(size, rect) } else { (rect.0, rect.1, 0, 0) };
                format!("{},pad={dw}:{dh}:{ox}:{oy}:color=black", top_left_scale(size, (fw, fh), !self.filter))
            }
            None => swscale_fit(rect, self.keep_aspect),
        };
        Some((filter, x as i64, y as i64))
    }
}

impl VideoEffect {
    /// The per-pixel filter of an effect that keeps the frame's geometry.
    fn pixel_filter(&self) -> Option<String> {
        match *self {
            Self::OpacityZoomPan(_) => None,
            // The processor subtracts the frame from white in RGB.
            Self::Invert => Some("format=rgb24,negate,format=yuv420p".into()),
            Self::BrightnessContrast { brightness, brightness_pre, contrast, saturation } => {
                let luma = format!("clip(256*((val/256-0.5+{brightness_pre})*{contrast}+0.5+{brightness})\\,0\\,255)");
                let chroma = format!("clip(256*(0.5+(val/256-0.5)*{saturation})\\,0\\,255)");
                Some(format!("lutyuv=y='{luma}':u='{chroma}':v='{chroma}'"))
            }
        }
    }

    /// The effect of a `<VIDEO_EFFECT` chunk, or `None` (with a warning) for a processor that
    /// is not one of the presets above.
    fn parse(chunk: &Chunk) -> Option<Self> {
        let code: Vec<String> = chunk
            .chunks("CODE")
            .flat_map(|c| &c.children)
            .filter_map(|n| match n {
                Node::Line(l) => Some(l.trim().strip_prefix('|').unwrap_or(l).trim().to_owned()),
                Node::Chunk(_) => None,
            })
            .collect();
        let code = code.join("\n");
        let parm: Vec<f64> = chunk
            .find_line("CODEPARM")
            .map(|t| t.iter().map(|v| v.parse().unwrap_or(0.0)).collect())
            .unwrap_or_default();
        // Parameter N of the code is the Nth value.
        let p = |n: usize| parm.get(n - 1).copied().unwrap_or(0.0);
        let effect = match fnv1a(code.trim_end()) {
            0x6d84a9bbe080548b if p(5) == 0.0 && p(9) == 0.0 && p(10) == 0.0 => {
                Some(Self::OpacityZoomPan(OpacityZoomPan {
                    opacity: p(1),
                    zoom: p(2),
                    x_offset: p(3),
                    y_offset: p(4),
                    filter: p(7) > 0.5,
                    clear: p(8) >= 0.5,
                    keep_aspect: p(11) != 0.0,
                }))
            }
            0xd74ef5e843cfeae2 => Some(Self::Invert),
            0xa63f9492d9ccbb20 => {
                let product = p(2) * p(5);
                let c = product.abs();
                let c = if c > 1.0 { (c - 1.0) * 4.0 + 1.0 } else { c };
                let sign = if product == 0.0 { 0.0 } else { product.signum() };
                Some(Self::BrightnessContrast {
                    brightness: p(1),
                    brightness_pre: p(4),
                    contrast: (c.exp2() - 1.0) * sign,
                    saturation: p(7),
                })
            }
            _ => None,
        };
        if effect.is_none() {
            let name = code.lines().next().unwrap_or_default();
            eprintln!("warning: video processor not rendered: {name}");
        }
        effect
    }
}

/// The size `size` takes inside `rect` keeping its aspect ratio, and its centred offset.
fn fit((sw, sh): (u32, u32), (w, h): (u32, u32)) -> (u32, u32, u32, u32) {
    let (fw, fh) = if sw as u64 * h as u64 > w as u64 * sh as u64 {
        (w, ((w as f64 * sh as f64 / sw as f64).round() as u32).max(2) & !1)
    } else {
        (((h as f64 * sw as f64 / sh as f64).round() as u32).max(2) & !1, h)
    };
    (fw, fh, ((w - fw) / 2) & !1, ((h - fh) / 2) & !1)
}

/// REAPER's scaler on YV12 planes: sample (x, y) of the output reads the source at
/// (x * sw / w, y * sh / h), bilinearly or from the pixel there, with no half-pixel centring.
/// Chroma planes use the luma ratio.
fn top_left_scale((sw, sh): (u32, u32), (w, h): (u32, u32), nearest: bool) -> String {
    if (sw, sh) == (w, h) {
        return "null".into();
    }
    let (sx, sy) = (sw as f64 / w as f64, sh as f64 / h as f64);
    let at = |pw: u32, ph: u32| {
        if nearest {
            format!("p(floor(X*{sx})\\,floor(Y*{sy}))")
        } else {
            format!("p(min(X*{sx}\\,{})\\,min(Y*{sy}\\,{}))", pw - 1, ph - 1)
        }
    };
    let (luma, chroma) = (at(sw, sh), at(sw.div_ceil(2), sh.div_ceil(2)));
    format!(
        "pad={}:{}:0:0,geq=lum='{luma}':cb='{chroma}':cr='{chroma}':interpolation=bilinear,crop={w}:{h}:0:0",
        sw.max(w).next_multiple_of(2),
        sh.max(h).next_multiple_of(2),
    )
}

/// FFmpeg's bilinear scaler into `rect`, for a source whose size is not known.
fn swscale_fit((w, h): (u32, u32), keep_aspect: bool) -> String {
    if keep_aspect {
        format!("scale={w}:{h}:flags=bilinear:force_original_aspect_ratio=decrease,pad={w}:{h}:-1:-1:color=black")
    } else {
        format!("scale={w}:{h}:flags=bilinear")
    }
}

/// A video item scaled to the whole frame, keeping its aspect ratio with black bars or
/// stretched, as REAPER draws items without a processor.
fn item_fit(size: Option<(u32, u32)>, frame: (u32, u32), keep_aspect: bool) -> String {
    let Some(size) = size else { return swscale_fit(frame, keep_aspect) };
    let (fw, fh, ox, oy) = if keep_aspect { fit(size, frame) } else { (frame.0, frame.1, 0, 0) };
    format!("{},pad={}:{}:{ox}:{oy}:color=black", top_left_scale(size, (fw, fh), false), frame.0, frame.1)
}

/// The frame size of a video file's first video stream, and how far its first decoding
/// timestamp precedes its start (the reordering delay of B-frames), read with `ffprobe`.
fn probe(path: &Path) -> Option<((u32, u32), f64)> {
    let output = crate::process::command("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-read_intervals", "%+#1"])
        .args(["-show_entries", "stream=width,height,start_time:packet=dts_time", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let value =
        |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')?.trim().parse::<f64>().ok());
    let (w, h) = (value("width")?, value("height")?);
    let delay = (value("start_time").unwrap_or(0.0) - value("dts_time").unwrap_or(0.0)).max(0.0);
    (w >= 1.0 && h >= 1.0).then_some(((w as u32, h as u32), delay))
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// The video processors of an FX chain's body (`BYPASS` lines, each followed by its plugin)
/// that are enabled.
fn chain_effects(nodes: &[Node]) -> Vec<VideoEffect> {
    let mut enabled = true;
    let mut effects = Vec::new();
    for node in nodes {
        match node {
            Node::Line(l) if line_key(l) == "BYPASS" => enabled = l.split_whitespace().nth(1) != Some("1"),
            Node::Chunk(c) if c.name() == "VIDEO_EFFECT" && enabled => effects.extend(VideoEffect::parse(c)),
            _ => {}
        }
    }
    effects
}

/// The video items of unmuted tracks that reach into `range`, bottom track first so that the
/// top track, which REAPER shows, is drawn last.
pub fn video_clips(project: &Project, range: TimeRange) -> Vec<VideoClip> {
    let mut clips = Vec::new();
    for track in project.tracks.iter().rev().filter(|t| !t.muted) {
        let slots = track.fx.iter().filter(|s| s.enabled);
        let track_effects: Vec<_> = slots.flat_map(|s| chain_effects(&s.raw)).collect();
        for item in track.items.iter().filter(|i| !i.muted) {
            let Some(take) = item.take() else { continue };
            let video_source = take.lines.get("SOURCE").is_some_and(|t| t.get(1).is_some_and(|k| k == "VIDEO"));
            let file = match &take.source {
                Source::File(path) if video_source => Some((path.clone(), 0.0)),
                source => video_file(source),
            };
            let Some((file, section_start)) = file else { continue };
            let (start, end) = (item.position.max(range.start), item.end().min(range.end));
            if end <= start {
                continue;
            }
            let rate = if take.playrate > 0.0 { take.playrate } else { 1.0 };
            let take_fx = take.extra.iter().filter_map(|n| match n {
                Node::Chunk(c) if c.name() == "TAKEFX" => Some(chain_effects(&c.children)),
                _ => None,
            });
            clips.push(VideoClip {
                path: scythe_core::rpp::resolve_media_path(project, &file),
                start: start - range.start,
                length: end - start,
                offset: section_start + take.source_offset + (start - item.position) * rate,
                rate,
                size: None,
                effects: take_fx.flatten().chain(track_effects.iter().cloned()).collect(),
            });
        }
    }
    clips
}

/// The video file a take plays and where its section starts: REAPER's `VIDEO` sources, or a
/// file source with a video extension.
fn video_file(source: &Source) -> Option<(PathBuf, f64)> {
    match source {
        Source::File(path) => is_video_file(path).then(|| (path.clone(), 0.0)),
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

/// The `key=value` pairs of a sink encoder option string. Tokens are separated by blanks and
/// may be quoted with `'`, `"` or `` ` ``; a token starting with `;` or `#` ends the list.
fn encoder_options(text: &str) -> Vec<(&str, &str)> {
    let mut options = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        let Some(first) = rest.chars().next() else { break };
        let token = match first {
            ';' | '#' => break,
            '\'' | '"' | '`' => {
                let body = &rest[1..];
                let end = body.find(first).unwrap_or(body.len());
                rest = body.get(end + 1..).unwrap_or_default();
                &body[..end]
            }
            _ => {
                let end = rest.find([' ', '\t', '\r', '\n']).unwrap_or(rest.len());
                let token = &rest[..end];
                rest = &rest[end..];
                token
            }
        };
        if let Some((key, value)) = token.split_once('=')
            && !key.is_empty()
            && !value.is_empty()
        {
            options.push((key, value));
        }
    }
    options
}

/// `ffmpeg` command-line options that are not encoder settings: an option string naming one
/// would change the command's inputs or outputs instead.
const NOT_ENCODER_OPTIONS: &[&str] = &[
    "i",
    "f",
    "y",
    "n",
    "t",
    "to",
    "ss",
    "sseof",
    "fs",
    "map",
    "filter",
    "filter_complex",
    "lavfi",
    "vf",
    "af",
    "c",
    "vcodec",
    "acodec",
    "an",
    "vn",
    "sn",
    "dn",
    "attach",
    "metadata",
    "filter_script",
];

/// The `ffmpeg` arguments for a sink option string, for the stream type `kind` (`v` or `a`):
/// `codec` picks the encoder, `tag` the codec tag, `q`/`qscale` a fixed quantizer, and
/// `range=jpeg` full-range video. Other pairs are encoder options, as REAPER passes them to
/// libavcodec.
fn option_args(text: &str, kind: char, encoder: &mut String) -> Vec<String> {
    let mut args = Vec::new();
    for (key, value) in encoder_options(text) {
        match key {
            "codec" => *encoder = value.to_owned(),
            "tag" => args.extend([format!("-tag:{kind}"), value.to_owned()]),
            "q" | "qscale" => args.extend([format!("-q:{kind}"), value.to_owned()]),
            "range" if kind == 'v' && value == "jpeg" => args.extend(["-color_range".into(), "pc".into()]),
            "range" => {}
            _ if key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !NOT_ENCODER_OPTIONS.contains(&key) =>
            {
                args.extend([format!("-{key}:{kind}"), value.to_owned()]);
            }
            _ => {}
        }
    }
    args
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
/// stream).
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
        // REAPER decodes video to YV12 and composites in it.
        let mut graph = String::from("[0:v]format=yuv420p[base0]");
        for (n, clip) in clips.iter().enumerate() {
            // REAPER draws a clip on the frames whose middle lies in it, its end included, each
            // showing the source frame on screen at that middle. Comparisons sit a quarter frame
            // early, clear of rounding.
            let first = (clip.start * fps - 0.5 - 1e-9).ceil() / fps;
            let end = ((clip.start + clip.length) * fps - 0.5 + 1e-9).floor().max(-1.0) / fps + 1.0 / fps;
            let micro = |t: f64| (t * 1e6).round() / 1e6;
            let (first, end) = (micro(first), micro(end));
            let (first_cut, end_cut) = (micro((first - 0.25 / fps).max(0.0)), micro(end - 0.25 / fps));
            let next = n + 1;
            let mut base = format!("base{n}");
            let placed = match clip.placement() {
                None => Some((item_fit(clip.size, (w, h), video.keep_aspect), 0, 0, 1.0)),
                Some(p) => {
                    if p.hides_background() {
                        graph.push_str(&format!(
                            ";[{base}]drawbox=c=black:t=fill:enable='gte(t,{first_cut})*lt(t,{end_cut})'[bg{n}]"
                        ));
                        base = format!("bg{n}");
                    }
                    p.placement(clip.size, (w, h)).map(|(filter, x, y)| (filter, x, y, p.alpha()))
                }
            };
            let Some((fit, x, y, alpha)) = placed else {
                graph.push_str(&format!(";[{base}]null[base{next}]"));
                continue;
            };
            // A second of the file before the clip's offset is decoded too, for the frame already
            // on screen there; the file's timestamps then map onto the render's, kept positive by
            // `lead` until the frame rate conversion picks the frames.
            let rate = clip.rate;
            let seek = (clip.offset - 1.0).max(0.0);
            args.extend(["-ss".into(), format!("{seek}"), "-t".into()]);
            args.push(format!("{}", clip.offset - seek + clip.length * rate + 1.0));
            args.extend(["-i".into(), clip.path.to_string_lossy().into_owned()]);
            let lead = 2.0 / rate + 1.0;
            let mut chain = String::from("format=yuv420p");
            for filter in clip.effects.iter().filter_map(VideoEffect::pixel_filter) {
                chain.push(',');
                chain.push_str(&filter);
            }
            let alpha = if alpha < 1.0 { format!(",lut=a='val*{alpha}'") } else { String::new() };
            graph.push_str(&format!(
                ";[{inputs}:v]{chain},{fit},setsar=1,\
                 setpts='({start}+{lead}+(PTS*TB+{seek}-{offset})/{rate})/TB',fps={num}/{den}:round=near,\
                 setpts=PTS-{lead}/TB,trim=start={first_cut}:end={end_cut},setpts=PTS-STARTPTS,\
                 format=yuva420p{alpha},tpad=start_duration={first}:color=black@0.0[clip{n}];\
                 [{base}][clip{n}]overlay=x={x}:y={y}:eof_action=repeat:enable='lt(t,{end_cut})'[base{next}]",
                start = clip.start,
                offset = clip.offset,
            ));
            inputs += 1;
        }
        args.extend(["-filter_complex".into(), graph]);
        maps.extend(["-map".into(), format!("[base{}]", clips.len())]);
        let mut codec_args: Vec<String> = video_args(codec, video.container).into_iter().map(String::from).collect();
        let options = option_args(&video.video_options, 'v', &mut codec_args[1]);
        maps.extend(codec_args);
        if !matches!(codec, VideoCodec::Ffv1 | VideoCodec::Huffyuv | VideoCodec::Dv | VideoCodec::Mjpeg) {
            maps.extend(["-b:v".into(), format!("{}k", video.video_kbps.max(1))]);
        }
        maps.extend(options);
        if codec == VideoCodec::Mjpeg {
            // The quality factor sets the quantizer, over any `q` option. REAPER's renders at
            // quality 100 reach quantizer 1, below FFmpeg's default floor of 2.
            let q = (100 - video.quality.min(100)) as f64 * 0.3;
            maps.extend(["-qmin".into(), "1".into(), "-q:v".into(), format!("{q}")]);
        }
        maps.extend(["-r".into(), format!("{num}/{den}")]);
    }
    if let Some(codec) = video.audio {
        let mut encoder = audio_encoder(codec).ok_or("the DV audio codec is not supported")?.to_owned();
        let wav = audio.ok_or("the render has no audio")?;
        args.extend(["-i".into(), wav.to_string_lossy().into_owned()]);
        let options = option_args(&video.audio_options, 'a', &mut encoder);
        maps.extend(["-map".into(), format!("{inputs}:a"), "-c:a".into(), encoder]);
        if !matches!(codec, AudioCodec::Pcm16 | AudioCodec::Pcm24 | AudioCodec::Float32) {
            maps.extend(["-b:a".into(), format!("{}k", video.audio_kbps.max(1))]);
        }
        maps.extend(options);
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
    // REAPER times the frames of a stream with B-frames by their decoding timestamps, so such a
    // video shows that delay ahead of the file's presentation times.
    let clips: Vec<VideoClip> = clips
        .iter()
        .map(|c| match probe(&c.path) {
            Some((size, delay)) => VideoClip { size: Some(size), offset: c.offset + delay, ..c.clone() },
            None => c.clone(),
        })
        .collect();
    let args = ffmpeg_args(video, &clips, duration, audio, out)?;
    let output = crate::process::command("ffmpeg")
        .args(&args)
        .output()
        .map_err(|e| format!("video renders need `ffmpeg` on PATH: {e}"))?;
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
            quality: 95,
            video_options: String::new(),
            audio_options: String::new(),
        };
        let args = ffmpeg_args(&v, &[], 2.0, Some(Path::new("a.wav")), Path::new("o.mkv")).unwrap().join(" ");
        assert_eq!(args, "-y -nostdin -v error -i a.wav -map 0:a -c:a pcm_s16le -t 2 -f matroska o.mkv");
        v.video = Some(VideoCodec::H264);
        v.audio = Some(AudioCodec::Aac);
        let args = ffmpeg_args(&v, &[], 2.0, Some(Path::new("a.wav")), Path::new("o.mkv")).unwrap().join(" ");
        assert!(args.contains("color=c=black:s=320x240:r=25/1:d=2"), "{args}");
        assert!(args.contains("-map 1:a -c:a aac -b:a 128k"), "{args}");
        assert!(args.contains("-c:v libx264 -pix_fmt yuv420p -b:v 2048k -r 25/1"), "{args}");
        v.video_options = "codec=libx265 crf=30 b=500000 range=jpeg tag=hev1 i=x ; g=5".into();
        v.audio_options = "'b=96000' q=2".into();
        let args = ffmpeg_args(&v, &[], 2.0, Some(Path::new("a.wav")), Path::new("o.mkv")).unwrap().join(" ");
        assert!(
            args.contains(
                "-c:v libx265 -pix_fmt yuv420p -b:v 2048k -crf:v 30 -b:v 500000 -color_range pc -tag:v hev1 -r"
            ),
            "{args}"
        );
        assert!(args.contains("-c:a aac -b:a 128k -b:a 96000 -q:a 2 -t"), "{args}");
        v.video = Some(VideoCodec::Mjpeg);
        v.video_options = "q=9".into();
        v.quality = 50;
        let args = ffmpeg_args(&v, &[], 2.0, Some(Path::new("a.wav")), Path::new("o.mkv")).unwrap().join(" ");
        assert!(args.contains("-c:v mjpeg -pix_fmt yuvj420p -q:v 9 -qmin 1 -q:v 15 -r"), "{args}");
        v.video = None;
        v.audio = None;
        assert!(ffmpeg_args(&v, &[], 2.0, None, Path::new("o.mkv")).is_err());
    }

    #[test]
    fn clips_show_on_the_frames_whose_middle_they_cover() {
        let v = Video {
            container: Container::Matroska,
            video: Some(VideoCodec::Ffv1),
            audio: None,
            video_kbps: 2048,
            audio_kbps: 128,
            width: 320,
            height: 240,
            fps: 25.0,
            keep_aspect: true,
            quality: 95,
            video_options: String::new(),
            audio_options: String::new(),
        };
        let clip = VideoClip {
            path: "a.mkv".into(),
            start: 0.25,
            length: 2.25,
            offset: 0.5,
            rate: 1.0,
            size: Some((160, 120)),
            effects: vec![],
        };
        let args = ffmpeg_args(&v, &[clip], 3.0, None, Path::new("o.mkv")).unwrap().join(" ");
        // Frames 6 (middle 0.26 s) to 62 (middle 2.5 s, the clip's end).
        assert!(args.contains("trim=start=0.23:end=2.51,") && args.contains("tpad=start_duration=0.24:"), "{args}");
        assert!(args.contains("-ss 0 -t 3.75 -i a.mkv"), "{args}");
        assert!(args.contains("geq=lum='p(min(X*0.5\\,159)\\,min(Y*0.5\\,119))':cb='p(min(X*0.5\\,79)"), "{args}");
    }

    #[test]
    fn geometry() {
        assert_eq!(fit((160, 120), (320, 180)), (240, 180, 40, 0));
        assert_eq!(fit((320, 120), (320, 240)), (320, 120, 0, 60));
        let p = OpacityZoomPan {
            opacity: 0.5,
            zoom: -10.0 * 2f64.log10(),
            x_offset: 0.5,
            y_offset: 0.0,
            filter: false,
            clear: true,
            keep_aspect: false,
        };
        // Half the frame, centred, then moved right by half of (320 + 160) / 2.
        let (filter, x, y) = p.placement(Some((320, 240)), (320, 240)).unwrap();
        assert_eq!((x, y), (200, 60));
        assert!(filter.contains("p(floor(X*2)\\,floor(Y*2))") && filter.contains("crop=160:120"), "{filter}");
        assert!(!p.hides_background());
        assert_eq!(OpacityZoomPan { opacity: 0.005, ..p.clone() }.placement(None, (320, 240)), None);
        assert!(OpacityZoomPan { opacity: 0.995, ..p }.hides_background());
    }

    #[test]
    fn unknown_processors_are_left_out() {
        let chain = scythe_core::rpp::parse(
            "<TAKEFX\nBYPASS 0 0 0\n<VIDEO_EFFECT \"Video processor\" \"\"\n<CODE\n|gfx_blit(0);\n>\nCODEPARM 1 0\n>\n>",
        )
        .unwrap();
        assert_eq!(chain_effects(&chain.children), []);
        let bc =
            VideoEffect::BrightnessContrast { brightness: 0.2, brightness_pre: 0.0, contrast: 1.0, saturation: 0.5 };
        assert_eq!(
            bc.pixel_filter().unwrap(),
            "lutyuv=y='clip(256*((val/256-0.5+0)*1+0.5+0.2)\\,0\\,255)':u='clip(256*(0.5+(val/256-0.5)*0.5)\\,0\\,255)':\
             v='clip(256*(0.5+(val/256-0.5)*0.5)\\,0\\,255)'"
        );
    }

    #[test]
    fn option_strings() {
        assert_eq!(encoder_options("  a=1\tb=\"c d\" \"e=f g\" x =y ;h=2"), [("a", "1"), ("b", "\"c"), ("e", "f g")]);
        assert_eq!(encoder_options("# all=comment"), []);
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
