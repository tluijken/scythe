//! Open Sound Control 1.0 messages and bundles: what goes over the UDP socket.

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Int(i32),
    Float(f32),
    Str(String),
}

impl Arg {
    /// A number argument as a float; strings that hold a number count as one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(*i as f64),
            Self::Float(f) => Some(*f as f64),
            Self::Str(s) => s.trim().parse().ok(),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub address: String,
    pub args: Vec<Arg>,
}

impl Message {
    pub fn new(address: impl Into<String>, args: Vec<Arg>) -> Self {
        Self { address: address.into(), args }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.address.len() + 16);
        push_str(&mut out, &self.address);
        let mut tags = String::from(",");
        for a in &self.args {
            tags.push(match a {
                Arg::Int(_) => 'i',
                Arg::Float(_) => 'f',
                Arg::Str(_) => 's',
            });
        }
        push_str(&mut out, &tags);
        for a in &self.args {
            match a {
                Arg::Int(i) => out.extend_from_slice(&i.to_be_bytes()),
                Arg::Float(f) => out.extend_from_slice(&f.to_be_bytes()),
                Arg::Str(s) => push_str(&mut out, s),
            }
        }
        out
    }
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// Packs messages into packets of at most `max` bytes: one message alone, several in a
/// `#bundle` with an immediate time tag. A message larger than `max` goes out on its own.
pub fn pack(messages: &[Message], max: usize) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut bundle: Vec<Vec<u8>> = Vec::new();
    let mut size = 16;
    let flush = |bundle: &mut Vec<Vec<u8>>, packets: &mut Vec<Vec<u8>>| match bundle.len() {
        0 => {}
        1 => packets.push(bundle.pop().unwrap_or_default()),
        _ => {
            let mut p = b"#bundle\0".to_vec();
            p.extend_from_slice(&1u64.to_be_bytes());
            for m in bundle.drain(..) {
                p.extend_from_slice(&(m.len() as u32).to_be_bytes());
                p.extend_from_slice(&m);
            }
            packets.push(p);
        }
    };
    for m in messages {
        let bytes = m.encode();
        if !bundle.is_empty() && size + 4 + bytes.len() > max {
            flush(&mut bundle, &mut packets);
            size = 16;
        }
        size += 4 + bytes.len();
        bundle.push(bytes);
    }
    flush(&mut bundle, &mut packets);
    packets
}

/// The messages in a packet (a message or a bundle, nested bundles included); `None` when it
/// is not valid OSC.
pub fn decode(packet: &[u8]) -> Option<Vec<Message>> {
    let mut out = Vec::new();
    decode_into(packet, &mut out, 0)?;
    Some(out)
}

fn decode_into(packet: &[u8], out: &mut Vec<Message>, depth: usize) -> Option<()> {
    if packet.starts_with(b"#bundle\0") {
        if depth > 8 {
            return None;
        }
        let mut at = 16;
        while at + 4 <= packet.len() {
            let len = u32::from_be_bytes(packet[at..at + 4].try_into().ok()?) as usize;
            at += 4;
            decode_into(packet.get(at..at + len)?, out, depth + 1)?;
            at += len;
        }
        return Some(());
    }
    let (address, mut at) = read_str(packet, 0)?;
    if !address.starts_with('/') {
        return None;
    }
    let mut args = Vec::new();
    if at < packet.len() {
        let (tags, next) = read_str(packet, at)?;
        at = next;
        for tag in tags.strip_prefix(',')?.chars() {
            let word = |at: usize| packet.get(at..at + 4).and_then(|b| b.try_into().ok());
            match tag {
                'i' => {
                    args.push(Arg::Int(i32::from_be_bytes(word(at)?)));
                    at += 4;
                }
                'f' => {
                    args.push(Arg::Float(f32::from_be_bytes(word(at)?)));
                    at += 4;
                }
                'd' => {
                    let b: [u8; 8] = packet.get(at..at + 8)?.try_into().ok()?;
                    args.push(Arg::Float(f64::from_be_bytes(b) as f32));
                    at += 8;
                }
                'h' => {
                    let b: [u8; 8] = packet.get(at..at + 8)?.try_into().ok()?;
                    args.push(Arg::Int(i64::from_be_bytes(b).clamp(i32::MIN as i64, i32::MAX as i64) as i32));
                    at += 8;
                }
                's' | 'S' => {
                    let (s, next) = read_str(packet, at)?;
                    args.push(Arg::Str(s));
                    at = next;
                }
                'T' => args.push(Arg::Int(1)),
                'F' | 'N' | 'I' => args.push(Arg::Int(0)),
                _ => return None,
            }
        }
    }
    out.push(Message { address, args });
    Some(())
}

fn read_str(packet: &[u8], at: usize) -> Option<(String, usize)> {
    let rest = packet.get(at..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    let s = String::from_utf8_lossy(&rest[..end]).into_owned();
    let next = at + (end + 4) / 4 * 4;
    Some((s, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_bundles_round_trip() {
        let m = Message::new("/track/1/name", vec![Arg::Str("Kick".into())]);
        let bytes = m.encode();
        assert_eq!(&bytes[..16], b"/track/1/name\0\0\0");
        assert_eq!(&bytes[16..20], b",s\0\0");
        assert_eq!(&bytes[20..], b"Kick\0\0\0\0");
        assert_eq!(decode(&bytes), Some(vec![m.clone()]));
        let f = Message::new("/track/1/volume", vec![Arg::Float(0.716)]);
        assert_eq!(&f.encode()[20..24], &0.716f32.to_be_bytes());
        let bare = b"/play\0\0\0";
        assert_eq!(decode(bare), Some(vec![Message::new("/play", vec![])]));
        let packets = pack(&[m.clone(), f.clone(), m.clone()], 80);
        assert_eq!(packets.len(), 2);
        assert!(packets[0].starts_with(b"#bundle\0"));
        let all: Vec<Message> = packets.iter().flat_map(|p| decode(p).unwrap()).collect();
        assert_eq!(all, vec![m.clone(), f, m]);
        assert_eq!(decode(b"nonsense"), None);
    }
}
