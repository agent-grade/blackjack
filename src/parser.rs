use vte::Perform;

/// `vte::Parser`, but handed only whole characters. vte 0.15 loses a byte when a
/// two-byte character is cut after its lead and a further multi-byte character
/// follows: `[0xc3]` then `[0xa9, b'/', 0xe4, 0xb8, 0xad]` reconstructs as
/// `é中` and the `/` never arrives. A pty read splits anywhere, so both models
/// would drift on a wide word at exactly the wrong offset. The tail of a
/// character a read cut in half waits here for the read that finishes it.
#[derive(Default)]
pub struct Parser {
    inner: vte::Parser,
    held: Vec<u8>,
    joined: Vec<u8>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) {
        if self.held.is_empty() {
            let whole = whole_chars(bytes);
            self.held.extend_from_slice(&bytes[whole..]);
            self.inner.advance(performer, &bytes[..whole]);
            return;
        }
        self.joined.clear();
        self.joined.extend_from_slice(&self.held);
        self.joined.extend_from_slice(bytes);
        self.held.clear();
        let whole = whole_chars(&self.joined);
        self.held.extend_from_slice(&self.joined[whole..]);
        self.inner.advance(performer, &self.joined[..whole]);
    }
}

/// How much of `bytes` ends on a character boundary. A character is at most
/// four bytes and its lead says how many, so the last lead byte decides; a byte
/// that leads nothing is passed on rather than held, or a stray one would wait
/// for a character that is never coming.
fn whole_chars(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let at = bytes.len() - back;
        let takes = match bytes[at] {
            0x00..=0x7f => break,
            0x80..=0xbf => continue,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => break,
        };
        return if back < takes { at } else { bytes.len() };
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_character_a_read_cut_in_half_is_held_back() {
        for (bytes, whole) in [
            (&b"abc"[..], 3),
            (b"a\xc3", 1),
            (b"a\xc3\xa9", 3),
            (b"a\xe4\xb8", 1),
            (b"a\xe4\xb8\xad", 4),
            (b"a\xf0\x9f\x99", 1),
            (b"a\xf0\x9f\x99\x82", 5),
            (b"a\x80", 2),
            (b"a\xc0", 2),
            (b"a\xf5", 2),
            (b"a\xff", 2),
            (b"", 0),
        ] {
            assert_eq!(whole_chars(bytes), whole, "for {bytes:?}");
        }
    }
}
