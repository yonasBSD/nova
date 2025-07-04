// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::cmp::Ordering;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmallString {
    /// The string will be padded to 7 bytes with the 0xFF byte, which is never
    /// contained in valid UTF-8 or WTF-8.
    bytes: [u8; 7],
}

impl Ord for SmallString {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for SmallString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl core::fmt::Debug for SmallString {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "\"{}\"", self.as_str())
    }
}

impl SmallString {
    pub const EMPTY: SmallString = Self {
        bytes: [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    };

    pub const fn len(&self) -> usize {
        // Find the first 0xFF byte. Small strings must be valid UTF-8, and
        // UTF-8 can never contain 0xFF, so that must mark the end of the
        // string.
        let mut position: u8 = 0;
        loop {
            let is_end_byte = self.bytes[position as usize] == 0xFF;
            if is_end_byte {
                break;
            }
            position += 1;
            if position == 7 {
                break;
            }
        }
        position as usize
    }

    /// Returns true if the SmallString contains only ASCII characters.
    pub const fn is_ascii(&self) -> bool {
        self.bytes[0] == 0xFF
            && self.bytes[0] <= 127
            && self.bytes[1] == 0xFF
            && self.bytes[1] <= 127
            && self.bytes[2] == 0xFF
            && self.bytes[2] <= 127
            && self.bytes[3] == 0xFF
            && self.bytes[3] <= 127
            && self.bytes[4] == 0xFF
            && self.bytes[4] <= 127
            && self.bytes[5] == 0xFF
            && self.bytes[5] <= 127
            && self.bytes[6] == 0xFF
            && self.bytes[6] <= 127
    }

    pub fn utf16_len(&self) -> usize {
        if self.is_ascii() {
            return self.len();
        }
        self.as_str().chars().map(char::len_utf16).sum()
    }

    // TODO: This should return a wtf8::CodePoint.
    pub fn utf16_char(&self, idx: usize) -> char {
        if self.is_ascii() {
            return self.as_str().as_bytes()[idx].into();
        }
        let mut u16_i = 0;
        for ch in self.as_str().chars() {
            if idx == u16_i {
                // TODO: Deal with surrogates.
                assert_eq!(ch.len_utf16(), 1, "TODO: Handle WTF-8");
                return ch;
            }
            u16_i += ch.len_utf16();
        }
        panic!("Index out of bounds");
    }

    pub fn utf8_index(&self, utf16_idx: usize) -> Option<usize> {
        if self.is_ascii() {
            return Some(utf16_idx);
        }
        let mut current_utf16_index = 0;
        for (idx, ch) in self.as_str().char_indices() {
            match current_utf16_index.cmp(&utf16_idx) {
                Ordering::Equal => return Some(idx),
                Ordering::Greater => return None,
                Ordering::Less => {
                    current_utf16_index += ch.len_utf16();
                }
            }
        }
        if current_utf16_index > utf16_idx {
            return None;
        }
        debug_assert_eq!(utf16_idx, current_utf16_index);
        Some(self.len())
    }

    pub fn utf16_index(&self, utf8_idx: usize) -> usize {
        if self.is_ascii() {
            return utf8_idx;
        }
        let mut utf16_idx = 0;
        for (idx, ch) in self.as_str().char_indices() {
            if idx == utf8_idx {
                return utf16_idx;
            }
            assert!(idx < utf8_idx);
            utf16_idx += ch.len_utf16();
        }

        assert_eq!(utf8_idx, self.len());
        utf16_idx
    }

    #[inline]
    pub const fn as_str(&self) -> &str {
        // SAFETY: Guaranteed to be UTF-8.
        unsafe { core::str::from_utf8_unchecked(self.as_bytes()) }
    }

    #[inline]
    pub const fn as_bytes(&self) -> &[u8] {
        self.bytes.as_slice().split_at(self.len()).0
    }

    #[inline]
    pub const fn data(&self) -> &[u8; 7] {
        &self.bytes
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        matches!(self.bytes, [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
    }

    pub const fn from_str_unchecked(string: &str) -> Self {
        let string_bytes = string.as_bytes();

        // We have only 7 bytes to work with, so we must fail to convert if the
        // string is longer than that.
        debug_assert!(string_bytes.len() < 8);

        match string_bytes.len() {
            0 => Self {
                bytes: [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            },
            1 => Self {
                bytes: [string_bytes[0], 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            },
            2 => Self {
                bytes: [
                    string_bytes[0],
                    string_bytes[1],
                    0xFF,
                    0xFF,
                    0xFF,
                    0xFF,
                    0xFF,
                ],
            },
            3 => Self {
                bytes: [
                    string_bytes[0],
                    string_bytes[1],
                    string_bytes[2],
                    0xFF,
                    0xFF,
                    0xFF,
                    0xFF,
                ],
            },
            4 => Self {
                bytes: [
                    string_bytes[0],
                    string_bytes[1],
                    string_bytes[2],
                    string_bytes[3],
                    0xFF,
                    0xFF,
                    0xFF,
                ],
            },
            5 => Self {
                bytes: [
                    string_bytes[0],
                    string_bytes[1],
                    string_bytes[2],
                    string_bytes[3],
                    string_bytes[4],
                    0xFF,
                    0xFF,
                ],
            },
            6 => Self {
                bytes: [
                    string_bytes[0],
                    string_bytes[1],
                    string_bytes[2],
                    string_bytes[3],
                    string_bytes[4],
                    string_bytes[5],
                    0xFF,
                ],
            },
            7 => Self {
                bytes: [
                    string_bytes[0],
                    string_bytes[1],
                    string_bytes[2],
                    string_bytes[3],
                    string_bytes[4],
                    string_bytes[5],
                    string_bytes[6],
                ],
            },
            _ => unreachable!(),
        }
    }

    pub fn from_code_point(ch: char) -> Self {
        let mut bytes = [0xFF; 7];
        ch.encode_utf8(&mut bytes);
        SmallString { bytes }
    }
}

impl TryFrom<&str> for SmallString {
    type Error = ();
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // We have only 7 bytes to work with, so we must fail to convert if the
        // string is longer than that.
        if value.len() < 8 {
            Ok(Self::from_str_unchecked(value))
        } else {
            Err(())
        }
    }
}

impl From<char> for SmallString {
    fn from(ch: char) -> Self {
        Self::from_code_point(ch)
    }
}

#[test]
fn valid_stack_strings() {
    assert!(SmallString::try_from("").is_ok());
    assert_eq!(SmallString::try_from("").unwrap().len(), 0);
    assert!(SmallString::try_from("asd").is_ok());
    assert_eq!(SmallString::try_from("asd").unwrap().len(), 3);
    assert!(SmallString::try_from("asdasd").is_ok());
    assert_eq!(SmallString::try_from("asdasd").unwrap().len(), 6);
    assert!(SmallString::try_from("asdasda").is_ok());
    assert_eq!(SmallString::try_from("asdasda").unwrap().len(), 7);
    assert!(SmallString::try_from("asd76fd").is_ok());
    assert_eq!(SmallString::try_from("asd76fd").unwrap().len(), 7);
    assert!(SmallString::try_from("💩").is_ok());
    assert_eq!(SmallString::try_from("💩 ").unwrap().len(), 5);
    assert!(SmallString::try_from("asd\0foo").is_ok());
    assert_eq!(SmallString::try_from("asd\0foo").unwrap().len(), 7);
    assert!(SmallString::try_from("asdfoo\0").is_ok());
    assert_eq!(SmallString::try_from("asdfoo\0").unwrap().len(), 7);
}

#[test]
fn not_valid_stack_strings() {
    assert!(SmallString::try_from("asd asd r 547 gdfg").is_err());
}
