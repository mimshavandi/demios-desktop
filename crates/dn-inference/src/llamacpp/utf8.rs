//! Tokens can split a UTF-8 character; hold bytes back until they form complete characters.

#[derive(Default)]
pub(super) struct Utf8Buf(Vec<u8>);

impl Utf8Buf {
    /// Append bytes; return the longest complete-character prefix, if any.
    pub(super) fn push(&mut self, bytes: &[u8]) -> Option<String> {
        self.0.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.0) {
            Ok(_) => self.0.len(),
            // An invalid sequence (not just an incomplete tail) is replaced, not held forever.
            Err(e) if e.error_len().is_some() => {
                let s = String::from_utf8_lossy(&self.0).into_owned();
                self.0.clear();
                return Some(s);
            }
            Err(e) => e.valid_up_to(),
        };
        if valid == 0 {
            return None;
        }
        let rest = self.0.split_off(valid);
        let s = String::from_utf8(std::mem::replace(&mut self.0, rest)).ok();
        s.filter(|s| !s.is_empty())
    }

    pub(super) fn finish(&mut self) -> String {
        let s = String::from_utf8_lossy(&self.0).into_owned();
        self.0.clear();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_character() {
        let e = "é".as_bytes();
        let mut b = Utf8Buf::default();
        assert_eq!(b.push(b"a"), Some("a".into()));
        assert_eq!(b.push(&e[..1]), None);
        assert_eq!(b.push(&e[1..]), Some("é".into()));
        assert_eq!(b.finish(), "");
    }

    #[test]
    fn invalid_bytes_replaced() {
        let mut b = Utf8Buf::default();
        assert_eq!(b.push(&[0xff, b'a']), Some("\u{fffd}a".into()));
    }
}
