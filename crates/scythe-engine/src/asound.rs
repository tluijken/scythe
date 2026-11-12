//! What the kernel says about the sound cards in `/proc/asound`, read without opening any PCM:
//! the cards REAPER's ALSA device lists offer, which card an ALSA device name means, who holds
//! a busy device, and what a USB interface can be opened with.

use std::path::{Path, PathBuf};

use crate::devices::{COMMON_RATES, Capabilities};

/// Where the kernel lists its sound cards.
pub const PROC_ASOUND: &str = "/proc/asound";

/// A sound card as `/proc/asound/cards` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub index: u32,
    /// The card's id (`QUADCAPTURE`), what `hw:<id>` names.
    pub id: String,
    /// The driver and the card's name (`USB-Audio - QUAD-CAPTURE`).
    pub name: String,
}

impl Card {
    /// The device name REAPER keeps for the card (`hw:QUADCAPTURE`).
    pub fn device(&self) -> String {
        format!("hw:{}", self.id)
    }
}

/// The cards in `/proc/asound/cards` text, in order.
pub fn parse_cards(text: &str) -> Vec<Card> {
    text.lines()
        .filter_map(|line| {
            let (index, rest) = line.trim_start().split_once(' ')?;
            let index = index.parse().ok()?;
            let rest = rest.trim_start().strip_prefix('[')?;
            let (id, rest) = rest.split_once(']')?;
            let name = rest.trim_start().strip_prefix(':')?.trim();
            Some(Card { index, id: id.trim().to_owned(), name: name.to_owned() })
        })
        .collect()
}

/// One PCM device as `/proc/asound/pcm` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub card: u32,
    pub device: u32,
    pub playback: bool,
    pub capture: bool,
}

/// The PCM devices in `/proc/asound/pcm` text (`01-00: USB Audio : USB Audio : playback 1 :
/// capture 1`).
pub fn parse_pcms(text: &str) -> Vec<Pcm> {
    text.lines()
        .filter_map(|line| {
            let (at, rest) = line.split_once(':')?;
            let (card, device) = at.trim().split_once('-')?;
            Some(Pcm {
                card: card.parse().ok()?,
                device: device.parse().ok()?,
                playback: rest.contains("playback "),
                capture: rest.contains("capture "),
            })
        })
        .collect()
}

/// The cards REAPER's ALSA input and output lists offer: every card with a PCM device, one entry
/// per card whatever its devices, the same list on both sides.
pub fn offered_cards(cards: &[Card], pcms: &[Pcm]) -> Vec<Card> {
    cards.iter().filter(|c| pcms.iter().any(|p| p.card == c.index)).cloned().collect()
}

/// The cards of this machine REAPER's ALSA lists offer.
pub fn system_cards() -> Vec<Card> {
    let read = |name: &str| std::fs::read_to_string(Path::new(PROC_ASOUND).join(name)).unwrap_or_default();
    offered_cards(&parse_cards(&read("cards")), &parse_pcms(&read("pcm")))
}

/// The card and device an ALSA device name opens, when it names one by card (`hw:1`,
/// `hw:QUADCAPTURE`, `plughw:CARD=1,DEV=0`, `front:CARD=QUADCAPTURE,DEV=0`); the card as written,
/// index or id.
fn card_and_device(name: &str) -> Option<(String, u32)> {
    let (_, args) = name.split_once(':')?;
    let mut card = None;
    let mut device = 0;
    for (i, arg) in args.split(',').enumerate() {
        let (key, value) = match arg.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (if i == 0 { "CARD" } else { "DEV" }, arg.trim()),
        };
        match key {
            "CARD" => card = Some(value.trim_matches('"').to_owned()),
            "DEV" => device = value.parse().ok()?,
            _ => {}
        }
    }
    card.filter(|c| !c.is_empty()).map(|c| (c, device))
}

/// The index of the card `card` (an index or an id) among `cards`.
fn card_index(card: &str, cards: &[Card]) -> Option<u32> {
    card.parse().ok().or_else(|| cards.iter().find(|c| c.id == card).map(|c| c.index))
}

/// The hardware device a name opens, as cpal enumerates it (`hw:CARD=1,DEV=0`); a REAPER card
/// name (`hw:QUADCAPTURE`) or a numbered one (`hw:1,0`). `None` for names that are no card's.
pub fn hardware_id(name: &str, cards: &[Card]) -> Option<String> {
    let prefix = name.split_once(':')?.0;
    if prefix != "hw" && prefix != "plughw" {
        return None;
    }
    let (card, device) = card_and_device(name)?;
    let index = card_index(&card, cards)?;
    Some(format!("{prefix}:CARD={index},DEV={device}"))
}

/// The card a device name belongs to, when it names one.
pub fn card_of(name: &str, cards: &[Card]) -> Option<Card> {
    let (card, _) = card_and_device(name)?;
    let index = card_index(&card, cards)?;
    cards.iter().find(|c| c.index == index).cloned()
}

/// A program holding a PCM device open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// Its process name (`pipewire`).
    pub program: String,
    /// Its command line, arguments separated by spaces.
    pub args: String,
}

/// Who holds the device `name` opens (playback, or with `input` capture), from the substreams'
/// `owner_pid` under `asound` and the owner's name under `proc`.
pub fn holder(name: &str, input: bool, asound: &Path, proc: &Path) -> Option<Holder> {
    let cards = parse_cards(&std::fs::read_to_string(asound.join("cards")).ok()?);
    let (card, device) = card_and_device(name)?;
    let index = card_index(&card, &cards)?;
    let dir: PathBuf = asound.join(format!("card{index}/pcm{device}{}", if input { 'c' } else { 'p' }));
    let mut subs: Vec<PathBuf> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).collect();
    subs.sort();
    subs.iter().find_map(|sub| {
        let status = std::fs::read_to_string(sub.join("status")).ok()?;
        let pid: u32 = status
            .lines()
            .find_map(|l| l.strip_prefix("owner_pid")?.trim_start_matches([' ', ':']).trim().parse().ok())?;
        let program = std::fs::read_to_string(proc.join(format!("{pid}/comm"))).unwrap_or_default().trim().to_owned();
        let args = std::fs::read(proc.join(format!("{pid}/cmdline"))).unwrap_or_default();
        let args = String::from_utf8_lossy(&args).trim_end_matches('\0').replace('\0', " ");
        Some(Holder { pid, program, args })
    })
}

/// Why the device `name` is busy and what to do about it, naming the program holding it when
/// the kernel says.
pub fn busy_message(name: &str, input: bool) -> String {
    busy_text(name, holder(name, input, Path::new(PROC_ASOUND), Path::new("/proc")).as_ref(), std::process::id())
}

fn busy_text(name: &str, holder: Option<&Holder>, own_pid: u32) -> String {
    let free = "Choose the interface on the PipeWire or JACK audio system instead, or free it for ALSA (turn \
                its profile Off in pavucontrol or with wpctl set-profile, or stop the program), then Apply again.";
    match holder {
        Some(h) if h.pid == own_pid => format!(
            "{name} is busy: Scythe itself still has it open. Close the other stream on it (input monitoring, \
             another device setting) and Apply again."
        ),
        Some(h) if matches!(h.program.as_str(), "pipewire" | "wireplumber" | "pipewire-pulse") => format!(
            "{name} is busy: PipeWire holds the interface (process {} {}). It lets go a few seconds after its \
             last stream stops. {free}",
            h.program, h.pid
        ),
        Some(h) if h.program == "pulseaudio" => {
            format!("{name} is busy: PulseAudio holds the interface (process {}). {free}", h.pid)
        }
        Some(h) if h.program.starts_with("scythe") => format!(
            "{name} is busy: another Scythe process holds it ({} {}, process {}). Quit it (a headless scythe-server or \
             scythe-mcp opens the saved device too), then Apply again.",
            h.program, h.args, h.pid
        ),
        Some(h) => format!("{name} is busy: {} (process {}) holds it. {free}", h.program, h.pid),
        None => format!("{name} is busy: another program or the sound server (PipeWire) has it open. {free}"),
    }
}

/// One stream format a USB audio interface offers, from `/proc/asound/cardN/streamM`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbFormat {
    /// Bits per sample (`S32_LE` is 32, `S24_3LE` 24).
    pub bits: u16,
    pub channels: u16,
    /// The rates it runs at, of those REAPER offers.
    pub rates: Vec<u32>,
}

/// The playback and the capture formats in a USB interface's `streamM` text.
pub fn parse_usb_stream(text: &str) -> (Vec<UsbFormat>, Vec<UsbFormat>) {
    let (mut playback, mut capture) = (Vec::new(), Vec::new());
    let mut side: Option<bool> = None;
    let mut current: Option<UsbFormat> = None;
    let mut flush = |current: &mut Option<UsbFormat>, side: Option<bool>| {
        if let (Some(f), Some(input)) = (current.take(), side) {
            if input { capture.push(f) } else { playback.push(f) }
        }
    };
    for line in text.lines() {
        let t = line.trim();
        if t == "Playback:" || t == "Capture:" {
            flush(&mut current, side);
            side = Some(t == "Capture:");
        } else if let Some(format) = t.strip_prefix("Format:") {
            flush(&mut current, side);
            current = Some(UsbFormat { bits: format_bits(format.trim()), channels: 0, rates: Vec::new() });
        } else if let (Some(f), Some(n)) = (current.as_mut(), t.strip_prefix("Channels:")) {
            f.channels = n.trim().parse().unwrap_or(0);
        } else if let (Some(f), Some(r)) = (current.as_mut(), t.strip_prefix("Rates:")) {
            f.rates = parse_rates(r);
        }
    }
    flush(&mut current, side);
    (playback, capture)
}

fn format_bits(format: &str) -> u16 {
    let digits: String = format.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

/// `44100, 48000` or `8000 - 96000 (continuous)`.
fn parse_rates(text: &str) -> Vec<u32> {
    let text = text.split('(').next().unwrap_or("").trim();
    if let Some((lo, hi)) = text.split_once('-') {
        let (lo, hi) = (lo.trim().parse().unwrap_or(0), hi.trim().parse().unwrap_or(0));
        return COMMON_RATES.into_iter().filter(|r| (lo..=hi).contains(r)).collect();
    }
    text.split(',').filter_map(|r| r.trim().parse().ok()).collect()
}

/// What a side of a USB interface can be opened with, from its formats.
pub fn usb_capabilities(formats: &[UsbFormat]) -> Option<Capabilities> {
    if formats.is_empty() {
        return None;
    }
    let mut channels: Vec<u16> = formats.iter().map(|f| f.channels).filter(|&c| c > 0).collect();
    channels.sort_unstable();
    channels.dedup();
    let mut sample_rates: Vec<u32> = formats.iter().flat_map(|f| f.rates.iter().copied()).collect();
    sample_rates.sort_unstable();
    sample_rates.dedup();
    let mut bits: Vec<u16> = formats.iter().map(|f| f.bits).filter(|&b| b > 0).collect();
    bits.sort_unstable();
    bits.dedup();
    let default_channels = channels.last().copied().unwrap_or(2);
    Some(Capabilities { channels, sample_rates, buffer_sizes: None, default_channels, bits })
}

/// What the device `name` can be opened with on the `input` side, as its USB interface tells the
/// kernel, for a device that cannot be asked because something holds it.
pub fn proc_capabilities(name: &str, input: bool, asound: &Path) -> Option<Capabilities> {
    let cards = parse_cards(&std::fs::read_to_string(asound.join("cards")).ok()?);
    let (card, device) = card_and_device(name)?;
    let index = card_index(&card, &cards)?;
    let text = std::fs::read_to_string(asound.join(format!("card{index}/stream{device}"))).ok()?;
    let (playback, capture) = parse_usb_stream(&text);
    usb_capabilities(if input { &capture } else { &playback })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS: &str = include_str!("asound_fixtures/cards.txt");
    const PCMS: &str = include_str!("asound_fixtures/pcm.txt");
    const QUAD: &str = include_str!("asound_fixtures/quadcapture_stream0.txt");

    #[test]
    fn the_cards_are_listed_as_reaper_lists_them() {
        let cards = offered_cards(&parse_cards(CARDS), &parse_pcms(PCMS));
        let shown: Vec<String> = cards.iter().map(|c| format!("{} ; {}", c.device(), c.name)).collect();
        assert_eq!(shown, ["hw:sofhdadsp ; sof-hda-dsp - sof-hda-dsp", "hw:QUADCAPTURE ; USB-Audio - QUAD-CAPTURE"]);
    }

    #[test]
    fn a_card_without_pcm_devices_is_not_offered() {
        let cards = parse_cards(
            " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n                      HDA Intel PCH at 0x1\n \
             3 [MidiOnly       ]: USB-Audio - Midi Keyboard\n                      Some MIDI keyboard\n",
        );
        let pcms = parse_pcms("00-00: ALC : ALC : playback 1 : capture 1\n00-03: HDMI 0 : HDMI 0 : playback 1\n");
        let offered = offered_cards(&cards, &pcms);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].device(), "hw:PCH");
    }

    #[test]
    fn device_names_resolve_to_the_numbered_hardware_device() {
        let cards = parse_cards(CARDS);
        assert_eq!(hardware_id("hw:QUADCAPTURE", &cards).as_deref(), Some("hw:CARD=1,DEV=0"));
        assert_eq!(hardware_id("hw:1,0", &cards).as_deref(), Some("hw:CARD=1,DEV=0"));
        assert_eq!(hardware_id("hw:sofhdadsp,3", &cards).as_deref(), Some("hw:CARD=0,DEV=3"));
        assert_eq!(hardware_id("plughw:CARD=QUADCAPTURE,DEV=0", &cards).as_deref(), Some("plughw:CARD=1,DEV=0"));
        assert_eq!(hardware_id("hw:CARD=1,DEV=0", &cards).as_deref(), Some("hw:CARD=1,DEV=0"));
        assert_eq!(hardware_id("hw:NOPE", &cards), None);
        assert_eq!(hardware_id("null", &cards), None);
        assert_eq!(hardware_id("front:CARD=QUADCAPTURE,DEV=0", &cards), None);
        assert_eq!(card_of("front:CARD=QUADCAPTURE,DEV=0", &cards).map(|c| c.index), Some(1));
    }

    #[test]
    fn the_quad_capture_stream_gives_its_one_format_per_side() {
        let (playback, capture) = parse_usb_stream(QUAD);
        assert_eq!(playback, [UsbFormat { bits: 32, channels: 4, rates: vec![44_100] }]);
        assert_eq!(capture, [UsbFormat { bits: 32, channels: 6, rates: vec![44_100] }]);
        let out = usb_capabilities(&playback).unwrap();
        assert_eq!(
            (out.channels, out.sample_rates, out.bits, out.default_channels),
            (vec![4], vec![44_100], vec![32], 4)
        );
        let inp = usb_capabilities(&capture).unwrap();
        assert_eq!((inp.channels, inp.default_channels), (vec![6], 6));
    }

    #[test]
    fn continuous_and_listed_rates_are_read() {
        assert_eq!(parse_rates(" 44100, 48000, 96000"), [44_100, 48_000, 96_000]);
        assert_eq!(parse_rates(" 8000 - 96000 (continuous)"), [44_100, 48_000, 88_200, 96_000]);
        assert_eq!(format_bits("S24_3LE"), 24);
    }

    #[test]
    fn capabilities_and_holders_come_from_the_proc_tree() {
        let dir = std::env::temp_dir().join(format!("scythe-asound-{}", std::process::id()));
        let sub = dir.join("asound/card1/pcm0p/sub0");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(dir.join("proc/4242")).unwrap();
        std::fs::write(dir.join("asound/cards"), CARDS).unwrap();
        std::fs::write(dir.join("asound/card1/stream0"), QUAD).unwrap();
        std::fs::write(sub.join("status"), "state: RUNNING\nowner_pid   : 4242\ntrigger_time: 1.0\n").unwrap();
        std::fs::write(dir.join("proc/4242/comm"), "pipewire\n").unwrap();
        let asound = dir.join("asound");
        let caps = proc_capabilities("hw:QUADCAPTURE", false, &asound).unwrap();
        assert_eq!(caps.channels, [4]);
        let held = holder("hw:CARD=1,DEV=0", false, &asound, &dir.join("proc"));
        assert_eq!(held, Some(Holder { pid: 4242, program: "pipewire".into(), args: String::new() }));
        assert_eq!(holder("hw:CARD=1,DEV=0", true, &asound, &dir.join("proc")), None);
        let text = busy_text("hw:QUADCAPTURE", held.as_ref(), 1);
        assert!(text.starts_with("hw:QUADCAPTURE is busy: PipeWire holds the interface (process pipewire 4242)"));
        assert!(text.contains("PipeWire or JACK"));
        assert!(busy_text("hw:X", held.as_ref(), 4242).contains("Scythe itself"));
        assert!(busy_text("hw:X", None, 1).contains("another program or the sound server"));
        let other = Holder { pid: 9, program: "scythe-server".into(), args: "scythe-server --control :8771".into() };
        let text = busy_text("hw:X", Some(&other), 1);
        assert!(text.contains("another Scythe process") && text.contains("--control :8771"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
