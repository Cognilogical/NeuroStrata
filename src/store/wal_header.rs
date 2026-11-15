#![allow(dead_code)] // consumed by tests only until Phase B wires it (#207)

//! WAL header rules shared by the recovery arms (`#207`).
//!
//! The 25-byte header the recovery code used to assume is only 25 bytes when
//! `enableChecksums` is on. `WAL::writeHeader` (`storage/wal/wal.cpp:394-402`)
//! writes `uuid databaseID` (16 bytes, `common::uuid` is an `int128_t`) plus
//! `bool enableChecksums` (1 byte, `Serializer::serializeValue<T>` writes
//! `sizeof(T)`), and `ChecksumWriter::onObjectEnd` (`checksum_writer.cpp:52-61`)
//! appends a `u64` checksum to that object only when checksums are enabled:
//!
//! ```text
//! checksums off: [16-byte id][flag]                              17 bytes
//! checksums on : [16-byte id][flag][u64 checksum]                25 bytes
//! ```
//!
//! So the flag lives at byte 16 **whenever the file is at least 17 bytes long**,
//! and 25 bytes are required only to trust a header whose flag says checksums
//! are on. Requiring 25 unconditionally turns a legitimate 17-byte
//! checksums-off WAL into an unusable header and would make Phase B refuse a
//! store that opens today.
//!
//! This module is pure: callers do the I/O and hand over bytes or lengths.

/// `uuid databaseID` (16) plus `bool enableChecksums` (1).
pub const WAL_HEADER_BYTES: u64 = 17;

/// The 17-byte header plus the `u64` checksum `ChecksumWriter` appends.
pub const WAL_HEADER_CHECKSUMMED_BYTES: u64 = 25;

/// Byte offset of the `enableChecksums` flag inside the header.
pub const CHECKSUM_FLAG_OFFSET: usize = 16;

/// What the leading bytes of a WAL file prove about its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderRead {
    /// No bytes at all: there is nothing to replay.
    Absent,
    /// Fewer bytes than the flag (17) or than a checksummed header (25) needs.
    TooShort { bytes: u64 },
    /// The flag was read, and the header is long enough to trust.
    Readable { checksums: bool, header_bytes: u64 },
}

/// Reads the header state from the first bytes of a WAL.
///
/// The flag is read at 17 bytes. When it says checksums are on, 25 bytes are
/// required, because the record walk needs to know whether every frame carries
/// an 8-byte trailer.
pub fn read_header(bytes: &[u8]) -> HeaderRead {
    let len = bytes.len() as u64;
    if len == 0 {
        return HeaderRead::Absent;
    }
    if len < WAL_HEADER_BYTES {
        return HeaderRead::TooShort { bytes: len };
    }
    let checksums = match bytes.get(CHECKSUM_FLAG_OFFSET) {
        Some(flag) => *flag != 0,
        None => return HeaderRead::TooShort { bytes: len },
    };
    if checksums && len < WAL_HEADER_CHECKSUMMED_BYTES {
        return HeaderRead::TooShort { bytes: len };
    }
    HeaderRead::Readable {
        checksums,
        header_bytes: if checksums {
            WAL_HEADER_CHECKSUMMED_BYTES
        } else {
            WAL_HEADER_BYTES
        },
    }
}

/// The `enableChecksums` flag, or `None` when the header cannot be trusted.
pub fn wal_checksum_flag(bytes: &[u8]) -> Option<bool> {
    match read_header(bytes) {
        HeaderRead::Readable { checksums, .. } => Some(checksums),
        HeaderRead::Absent | HeaderRead::TooShort { .. } => None,
    }
}

/// One WAL side as observed by the header pre-flight. The caller supplies the
/// length and the flag it read with [`wal_checksum_flag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderProbe {
    pub exists: bool,
    pub bytes: u64,
    pub checksums: Option<bool>,
}

impl HeaderProbe {
    pub const ABSENT: Self = Self {
        exists: false,
        bytes: 0,
        checksums: None,
    };

    pub const fn present(bytes: u64, checksums: Option<bool>) -> Self {
        Self {
            exists: true,
            bytes,
            checksums,
        }
    }

    /// A probe built directly from the bytes the caller read.
    ///
    /// An empty file is [`HeaderProbe::ABSENT`], not `present(0, None)`: a
    /// 0-byte `.wal` with no `.wal.checkpoint` must classify as `NoWal`, not as
    /// an unusable header the open would refuse.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::ABSENT;
        }
        Self::present(bytes.len() as u64, wal_checksum_flag(bytes))
    }

    /// Usable means the flag is readable **and** the file is long enough for the
    /// framing that flag implies: 17 bytes for checksums-off, 25 for
    /// checksums-on.
    pub const fn usable(self) -> bool {
        if !self.exists {
            return false;
        }
        match self.checksums {
            Some(false) => self.bytes >= WAL_HEADER_BYTES,
            Some(true) => self.bytes >= WAL_HEADER_CHECKSUMMED_BYTES,
            None => false,
        }
    }
}

/// Which side the checksum configuration should be read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderSource {
    /// Neither WAL exists.
    None,
    /// Exactly one side is usable, or both agree.
    Single { checksums: bool, frozen_only: bool },
    /// Refuse: the pair cannot be served by one `enable_checksums` setting.
    Conflict(HeaderConflict),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderConflict {
    /// At least one WAL exists but no side carries a usable header. Before
    /// `#207` this silently defaulted to checksums-on, which mis-parses a
    /// checksums-off WAL.
    NoUsableHeader,
    /// Both sides are readable and disagree. The engine replays the frozen WAL
    /// first (`wal_replayer.cpp:337-349`), so any single `enable_checksums`
    /// value mis-parses one side.
    ChecksumsDisagree,
}

/// The `#207` header rule: pick by a *usable* header, never by existence. A
/// 0-byte or truncated active `.wal` no longer hides the frozen
/// `.wal.checkpoint`, and a mixed pair is refused instead of defaulting to
/// checksums-on.
pub const fn select_header_source(active: HeaderProbe, frozen: HeaderProbe) -> HeaderSource {
    let (active_usable, frozen_usable) = (active.usable(), frozen.usable());
    match (active_usable, frozen_usable) {
        (true, true) => {
            let (a, f) = match (active.checksums, frozen.checksums) {
                (Some(a), Some(f)) => (a, f),
                // `usable()` guarantees both are `Some`.
                _ => return HeaderSource::Conflict(HeaderConflict::NoUsableHeader),
            };
            if a == f {
                HeaderSource::Single {
                    checksums: a,
                    frozen_only: false,
                }
            } else {
                HeaderSource::Conflict(HeaderConflict::ChecksumsDisagree)
            }
        }
        // Only the frozen side is usable: the case `#207` fixes.
        (false, true) => HeaderSource::Single {
            checksums: match frozen.checksums {
                Some(flag) => flag,
                None => true,
            },
            frozen_only: true,
        },
        (true, false) => HeaderSource::Single {
            checksums: match active.checksums {
                Some(flag) => flag,
                None => true,
            },
            frozen_only: false,
        },
        (false, false) => {
            if active.exists || frozen.exists {
                HeaderSource::Conflict(HeaderConflict::NoUsableHeader)
            } else {
                HeaderSource::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(checksums: bool, extra: usize) -> Vec<u8> {
        let mut out = vec![0x2au8; CHECKSUM_FLAG_OFFSET];
        out.push(u8::from(checksums));
        if checksums {
            out.extend_from_slice(&[0u8; 8]);
        }
        out.extend(std::iter::repeat(0u8).take(extra));
        out
    }

    #[test]
    fn the_flag_is_read_at_seventeen_bytes() {
        assert_eq!(
            read_header(&header(false, 0)),
            HeaderRead::Readable {
                checksums: false,
                header_bytes: 17
            }
        );
    }

    #[test]
    fn a_checksummed_header_needs_twenty_five_bytes() {
        assert_eq!(
            read_header(&header(true, 0)),
            HeaderRead::Readable {
                checksums: true,
                header_bytes: 25
            }
        );
        assert_eq!(
            read_header(&header(true, 0)[..24]),
            HeaderRead::TooShort { bytes: 24 }
        );
    }

    #[test]
    fn a_sixteen_byte_file_has_no_readable_flag() {
        assert_eq!(read_header(&[0u8; 16]), HeaderRead::TooShort { bytes: 16 });
        assert_eq!(wal_checksum_flag(&[0u8; 16]), None);
    }

    #[test]
    fn an_empty_file_is_absent_not_short() {
        assert_eq!(read_header(&[]), HeaderRead::Absent);
        assert_eq!(wal_checksum_flag(&[]), None);
    }

    #[test]
    fn a_seventeen_byte_checksums_off_wal_is_a_usable_header_source() {
        assert_eq!(
            select_header_source(HeaderProbe::present(17, Some(false)), HeaderProbe::ABSENT),
            HeaderSource::Single {
                checksums: false,
                frozen_only: false
            }
        );
    }

    #[test]
    fn header_source_uses_the_only_usable_side() {
        let source = select_header_source(
            // A 0-byte active `.wal` must not hide the frozen one (#207).
            HeaderProbe::present(0, None),
            HeaderProbe::present(3103, Some(true)),
        );
        assert_eq!(
            source,
            HeaderSource::Single {
                checksums: true,
                frozen_only: true
            }
        );
    }

    #[test]
    fn header_source_accepts_a_pair_that_agrees() {
        let source = select_header_source(
            HeaderProbe::present(1100, Some(false)),
            HeaderProbe::present(3103, Some(false)),
        );
        assert_eq!(
            source,
            HeaderSource::Single {
                checksums: false,
                frozen_only: false
            }
        );
    }

    #[test]
    fn header_source_refuses_a_mixed_pair() {
        let source = select_header_source(
            HeaderProbe::present(1100, Some(true)),
            HeaderProbe::present(3103, Some(false)),
        );
        assert_eq!(
            source,
            HeaderSource::Conflict(HeaderConflict::ChecksumsDisagree)
        );
    }

    #[test]
    fn header_source_refuses_a_short_active_with_no_frozen_peer() {
        let source = select_header_source(HeaderProbe::present(12, None), HeaderProbe::ABSENT);
        assert_eq!(
            source,
            HeaderSource::Conflict(HeaderConflict::NoUsableHeader)
        );
    }

    #[test]
    fn header_source_is_none_for_two_zero_byte_files() {
        // A 0-byte `.wal` must not become `Conflict(NoUsableHeader)`.
        assert_eq!(
            select_header_source(HeaderProbe::from_bytes(&[]), HeaderProbe::from_bytes(&[])),
            HeaderSource::None
        );
    }

    #[test]
    fn header_source_is_none_only_when_neither_wal_exists() {
        assert_eq!(
            select_header_source(HeaderProbe::ABSENT, HeaderProbe::ABSENT),
            HeaderSource::None
        );
    }
}
