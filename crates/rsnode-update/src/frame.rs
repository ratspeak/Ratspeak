//! Management commands carried as Link DATA between a manager's client and the node.
//!
//! Request: `version(1) id(4, BE) command(1) body`. Response: `version(1) id(4) result(1) body`.
//! The endpoint requires increasing nonzero IDs per authenticated Link and drops replayed or
//! reordered requests; these codecs enforce neither identity nor ordering. Start a new session
//! before IDs wrap.
//!
//! Firmware moves as `ota.chunk` requests. The endpoint accepts contiguous bytes, skips already
//! received prefixes and reports gaps; the codecs do not validate firmware or transfer ownership.
//! This module covers the transfer set; other command codes decode as [`RequestError::Unsupported`].

use crate::be32;
use crate::manifest::OTA_MANIFEST_LEN;

pub const LINK_COMMAND_VERSION: u8 = 1;
pub const COMMAND_HEADER_LEN: usize = 6;
/// Maximum encoded management request length, including its header.
pub const COMMAND_MAX_LEN: usize = 431;
/// Image or patch bytes one `ota.chunk` carries after its flags and offset.
pub const OTA_CHUNK_DATA_MAX: usize = COMMAND_MAX_LEN - COMMAND_HEADER_LEN - 5;
/// Bytes of an `ota.begin` body for a delta: manifest, then the base image and patch length.
pub const OTA_BEGIN_DELTA_LEN: usize = OTA_MANIFEST_LEN + DELTA_BASE_LEN;
/// `ota.chunk` flag: answer with the next expected offset.
pub const CHUNK_FLAG_ACK: u8 = 0x01;

pub const CMD_STATUS: u8 = 0x01;
pub const CMD_OTA_BEGIN: u8 = 0x08;
pub const CMD_OTA_CHUNK: u8 = 0x09;
pub const CMD_OTA_FINISH: u8 = 0x0A;
pub const CMD_OTA_ABORT: u8 = 0x0B;

const DELTA_BASE_LEN: usize = 40;

/// Expected base image and patch length. The command codec does not verify either.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeltaBase {
    pub base_len: u32,
    pub base_sha256: [u8; 32],
    pub patch_len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command<'a> {
    Status,
    /// Request a new or resumed image/patch transfer. Decoding checks body shape, not the
    /// manifest's signature, base image or transfer policy.
    OtaBegin {
        manifest: &'a [u8; OTA_MANIFEST_LEN],
        delta: Option<DeltaBase>,
    },
    OtaChunk {
        ack: bool,
        offset: u32,
        data: &'a [u8],
    },
    OtaFinish,
    OtaAbort,
}

impl<'a> Command<'a> {
    /// Decode a transfer body. `None` for a code outside the transfer set; a transfer code
    /// with a body of the wrong shape is [`RequestError::Malformed`].
    pub fn decode(code: u8, body: &'a [u8]) -> Option<Result<Self, RequestError>> {
        Some(Ok(match (code, body.len()) {
            (CMD_STATUS, 0) => Self::Status,
            (CMD_OTA_BEGIN, OTA_MANIFEST_LEN | OTA_BEGIN_DELTA_LEN) => {
                let (manifest, delta) = body.split_at(OTA_MANIFEST_LEN);
                let Ok(manifest) = manifest.try_into() else {
                    return Some(Err(RequestError::Malformed));
                };
                Self::OtaBegin {
                    manifest,
                    delta: (!delta.is_empty()).then(|| DeltaBase {
                        base_len: be32(&delta[..4]),
                        base_sha256: delta[4..36].try_into().unwrap_or([0; 32]),
                        patch_len: be32(&delta[36..]),
                    }),
                }
            }
            (CMD_OTA_CHUNK, 6..) if body[0] & !CHUNK_FLAG_ACK == 0 => Self::OtaChunk {
                ack: body[0] & CHUNK_FLAG_ACK != 0,
                offset: be32(&body[1..5]),
                data: &body[5..],
            },
            (CMD_OTA_FINISH, 0) => Self::OtaFinish,
            (CMD_OTA_ABORT, 0) => Self::OtaAbort,
            (CMD_STATUS | CMD_OTA_BEGIN | CMD_OTA_CHUNK | CMD_OTA_FINISH | CMD_OTA_ABORT, _) => {
                return Some(Err(RequestError::Malformed));
            }
            _ => return None,
        }))
    }

    pub const fn code(&self) -> u8 {
        match self {
            Self::Status => CMD_STATUS,
            Self::OtaBegin { .. } => CMD_OTA_BEGIN,
            Self::OtaChunk { .. } => CMD_OTA_CHUNK,
            Self::OtaFinish => CMD_OTA_FINISH,
            Self::OtaAbort => CMD_OTA_ABORT,
        }
    }

    /// Body length; an empty or oversized chunk is `BufferTooSmall`.
    pub fn body_len(&self) -> Result<usize, EncodeError> {
        Ok(match self {
            Self::Status | Self::OtaFinish | Self::OtaAbort => 0,
            Self::OtaBegin { delta, .. } => {
                if delta.is_some() {
                    OTA_BEGIN_DELTA_LEN
                } else {
                    OTA_MANIFEST_LEN
                }
            }
            Self::OtaChunk { data, .. } => {
                if data.is_empty() || data.len() > OTA_CHUNK_DATA_MAX {
                    return Err(EncodeError::BufferTooSmall);
                }
                5 + data.len()
            }
        })
    }

    /// Write the body into `body`, which must be exactly [`Self::body_len`] bytes.
    pub fn encode_body(&self, body: &mut [u8]) {
        match *self {
            Self::OtaBegin { manifest, delta } => {
                body[..OTA_MANIFEST_LEN].copy_from_slice(manifest);
                if let Some(delta) = delta {
                    let tail = &mut body[OTA_MANIFEST_LEN..];
                    tail[..4].copy_from_slice(&delta.base_len.to_be_bytes());
                    tail[4..36].copy_from_slice(&delta.base_sha256);
                    tail[36..].copy_from_slice(&delta.patch_len.to_be_bytes());
                }
            }
            Self::OtaChunk { ack, offset, data } => {
                body[0] = if ack { CHUNK_FLAG_ACK } else { 0 };
                body[1..5].copy_from_slice(&offset.to_be_bytes());
                body[5..].copy_from_slice(data);
            }
            Self::Status | Self::OtaFinish | Self::OtaAbort => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request<'a> {
    pub id: u32,
    pub command: Command<'a>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    /// Not a command this node can read; no reply is owed.
    Malformed,
    /// Valid header with an unknown or retired command code; its body is not validated.
    Unsupported { id: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CommandResult {
    Ok = 0,
    /// The proven identity's role does not allow the command.
    Denied = 1,
    Invalid = 2,
    Full = 3,
    NotFound = 4,
    /// Work is pending or the endpoint is temporarily unable to run this command; retry later.
    Busy = 5,
    Unsupported = 6,
    Storage = 7,
    /// The manifest or image failed verification.
    Rejected = 8,
    /// Policy refuses it: nearby-only on this path, or a change only the device may confirm.
    Refused = 9,
    /// Not possible on this node now: no release key, or updates blocked.
    Unavailable = 10,
    /// The chunk is not at the next offset; the body carries the offset expected.
    Gap = 11,
}

impl CommandResult {
    pub const fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Ok,
            1 => Self::Denied,
            2 => Self::Invalid,
            3 => Self::Full,
            4 => Self::NotFound,
            5 => Self::Busy,
            6 => Self::Unsupported,
            7 => Self::Storage,
            8 => Self::Rejected,
            9 => Self::Refused,
            10 => Self::Unavailable,
            11 => Self::Gap,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    BufferTooSmall,
}

/// Check version 1, a nonzero ID and the total length bound; return `(id, code, body)`.
pub fn decode_header(bytes: &[u8]) -> Result<(u32, u8, &[u8]), RequestError> {
    if !(COMMAND_HEADER_LEN..=COMMAND_MAX_LEN).contains(&bytes.len())
        || bytes[0] != LINK_COMMAND_VERSION
    {
        return Err(RequestError::Malformed);
    }
    let id = be32(&bytes[1..5]);
    if id == 0 {
        return Err(RequestError::Malformed);
    }
    Ok((id, bytes[5], &bytes[COMMAND_HEADER_LEN..]))
}

/// Decode a transfer request within [`COMMAND_MAX_LEN`]. Manifest signatures and delta metadata
/// need separate validation. The caller must bind the request to an authenticated session and
/// enforce ordering.
pub fn decode_request(bytes: &[u8]) -> Result<Request<'_>, RequestError> {
    let (id, code, body) = decode_header(bytes)?;
    match Command::decode(code, body) {
        Some(command) => Ok(Request {
            id,
            command: command?,
        }),
        None => Err(RequestError::Unsupported { id }),
    }
}

/// Encode a request, leaving `out` unchanged on error. Empty or oversized chunks return
/// `BufferTooSmall` even with a large output. ID zero can be encoded but is rejected by
/// [`decode_request`].
pub fn encode_request(request: &Request<'_>, out: &mut [u8]) -> Result<usize, EncodeError> {
    let len = COMMAND_HEADER_LEN + request.command.body_len()?;
    let out = out.get_mut(..len).ok_or(EncodeError::BufferTooSmall)?;
    encode_header(request.id, request.command.code(), out)?;
    request.command.encode_body(&mut out[COMMAND_HEADER_LEN..]);
    Ok(len)
}

/// Write `version id code`; the body follows at [`COMMAND_HEADER_LEN`]. A short output is
/// unchanged.
pub fn encode_header(id: u32, code: u8, out: &mut [u8]) -> Result<usize, EncodeError> {
    let out = out
        .get_mut(..COMMAND_HEADER_LEN)
        .ok_or(EncodeError::BufferTooSmall)?;
    out[0] = LINK_COMMAND_VERSION;
    out[1..5].copy_from_slice(&id.to_be_bytes());
    out[5] = code;
    Ok(COMMAND_HEADER_LEN)
}

/// Write a response header. Does not validate the ID or body.
pub fn encode_response_header(
    id: u32,
    result: CommandResult,
    out: &mut [u8],
) -> Result<usize, EncodeError> {
    encode_header(id, result as u8, out)
}

/// Split a version-1 response with a known result byte into ID, result and unvalidated body.
/// Does not reject ID zero, cap total length, correlate a request or authenticate the response.
pub fn decode_response(bytes: &[u8]) -> Option<(u32, CommandResult, &[u8])> {
    if bytes.len() < COMMAND_HEADER_LEN || bytes[0] != LINK_COMMAND_VERSION {
        return None;
    }
    Some((
        be32(&bytes[1..5]),
        CommandResult::from_byte(bytes[5])?,
        &bytes[COMMAND_HEADER_LEN..],
    ))
}

/// Offset replies (`ota.begin`, acknowledged or out-of-order `ota.chunk`): `next_offset(4)`;
/// a begin adds `chunk_max(2)`.
pub fn encode_offset(
    next_offset: u32,
    chunk_max: Option<u16>,
    out: &mut [u8],
) -> Result<usize, EncodeError> {
    let len = if chunk_max.is_some() { 6 } else { 4 };
    let out = out.get_mut(..len).ok_or(EncodeError::BufferTooSmall)?;
    out[..4].copy_from_slice(&next_offset.to_be_bytes());
    if let Some(chunk_max) = chunk_max {
        out[4..].copy_from_slice(&chunk_max.to_be_bytes());
    }
    Ok(len)
}

/// Decode an exact 4- or 6-byte offset reply without validating transfer bounds or chunk size.
pub fn decode_offset(body: &[u8]) -> Option<(u32, Option<u16>)> {
    match body.len() {
        4 => Some((be32(body), None)),
        6 => Some((be32(&body[..4]), Some(crate::be16(&body[4..])))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_and_reject_malformed_input() {
        let manifest = [0x5A; OTA_MANIFEST_LEN];
        let data = [0xC7; OTA_CHUNK_DATA_MAX];
        let commands = [
            Command::Status,
            Command::OtaBegin {
                manifest: &manifest,
                delta: None,
            },
            Command::OtaBegin {
                manifest: &manifest,
                delta: Some(DeltaBase {
                    base_len: 1_800_000,
                    base_sha256: [3; 32],
                    patch_len: 180_000,
                }),
            },
            Command::OtaChunk {
                ack: true,
                offset: 4_096,
                data: &data,
            },
            Command::OtaChunk {
                ack: false,
                offset: 0,
                data: &data[..1],
            },
            Command::OtaFinish,
            Command::OtaAbort,
        ];
        for (index, command) in commands.into_iter().enumerate() {
            let request = Request {
                id: 1 + index as u32,
                command,
            };
            let mut out = [0u8; COMMAND_MAX_LEN];
            let len = encode_request(&request, &mut out).unwrap();
            assert!(len <= COMMAND_MAX_LEN);
            assert_eq!(decode_request(&out[..len]), Ok(request));
            // Shorter chunks stay valid; every other body has one exact size.
            if !matches!(command, Command::OtaChunk { .. }) {
                assert_eq!(
                    decode_request(&out[..len - usize::from(len > COMMAND_HEADER_LEN)]).is_err(),
                    len > COMMAND_HEADER_LEN
                );
            }
        }
        assert_eq!(
            decode_request(&[LINK_COMMAND_VERSION, 0, 0, 0, 0, CMD_STATUS]),
            Err(RequestError::Malformed)
        );
        assert_eq!(
            decode_request(&[2, 0, 0, 0, 1, CMD_STATUS]),
            Err(RequestError::Malformed)
        );
        assert_eq!(
            decode_request(&[LINK_COMMAND_VERSION, 0, 0, 1, 2, 0x7F]),
            Err(RequestError::Unsupported { id: 258 })
        );
        // Codes outside the transfer set are unsupported here, whatever their body.
        assert_eq!(
            decode_request(&[LINK_COMMAND_VERSION, 0, 0, 0, 3, 0x02]),
            Err(RequestError::Unsupported { id: 3 })
        );
        assert_eq!(
            decode_request(&[LINK_COMMAND_VERSION, 0, 0, 0, 1, CMD_STATUS, 0]),
            Err(RequestError::Malformed)
        );
        let mut small = [0u8; 8];
        assert_eq!(
            encode_request(
                &Request {
                    id: 1,
                    command: Command::OtaBegin {
                        manifest: &manifest,
                        delta: None
                    }
                },
                &mut small
            ),
            Err(EncodeError::BufferTooSmall)
        );
        assert_eq!(small, [0; 8]);
        // Chunks carry data and only the ack flag.
        assert_eq!(
            decode_request(&[
                LINK_COMMAND_VERSION,
                0,
                0,
                0,
                1,
                CMD_OTA_CHUNK,
                1,
                0,
                0,
                0,
                0
            ]),
            Err(RequestError::Malformed)
        );
        assert_eq!(
            decode_request(&[
                LINK_COMMAND_VERSION,
                0,
                0,
                0,
                1,
                CMD_OTA_CHUNK,
                2,
                0,
                0,
                0,
                0,
                7
            ]),
            Err(RequestError::Malformed)
        );
        let too_long = [0u8; OTA_CHUNK_DATA_MAX + 1];
        assert!(
            encode_request(
                &Request {
                    id: 1,
                    command: Command::OtaChunk {
                        ack: false,
                        offset: 0,
                        data: &too_long,
                    },
                },
                &mut [0u8; 512],
            )
            .is_err()
        );
        let mut long_request = [0u8; COMMAND_MAX_LEN + 1];
        long_request[..COMMAND_HEADER_LEN].copy_from_slice(&[
            LINK_COMMAND_VERSION,
            0,
            0,
            0,
            1,
            CMD_OTA_CHUNK,
        ]);
        assert_eq!(decode_request(&long_request), Err(RequestError::Malformed));
        assert!(decode_request(&long_request[..COMMAND_MAX_LEN]).is_ok());
    }

    #[test]
    fn offsets_and_responses_round_trip() {
        let mut out = [0u8; 8];
        let len = encode_offset(8_192, Some(OTA_CHUNK_DATA_MAX as u16), &mut out).unwrap();
        assert_eq!(
            decode_offset(&out[..len]),
            Some((8_192, Some(OTA_CHUNK_DATA_MAX as u16)))
        );
        let len = encode_offset(7, None, &mut out).unwrap();
        assert_eq!(decode_offset(&out[..len]), Some((7, None)));
        assert_eq!(decode_offset(&out[..3]), None);
        let header = encode_response_header(9, CommandResult::Denied, &mut out).unwrap();
        assert_eq!(
            decode_response(&out[..header]),
            Some((9, CommandResult::Denied, &[][..]))
        );
        assert_eq!(
            decode_response(&[LINK_COMMAND_VERSION, 0, 0, 0, 1, 12]),
            None
        );
        for byte in 0..=11 {
            assert_eq!(CommandResult::from_byte(byte).map(|r| r as u8), Some(byte));
        }
    }
}
