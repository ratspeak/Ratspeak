//! Firmware-transfer wire formats shared by rsNode, `rsnode-client` and Ratspeak.
//!
//! A client opens a Link to the node's `rsnode.manage` destination, reads [`status`], then sends
//! `ota.begin`, chunks paced by [`upload`], and `ota.finish` ([`frame`]). The image travels whole
//! or as an RSDP patch ([`delta`]) against the running image, behind a signed `.rsota`
//! [`manifest`]. The node alone verifies signatures; nothing here authenticates anything.

#![no_std]

#[cfg(any(test, feature = "alloc"))]
extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod delta;
pub mod frame;
pub mod manifest;
pub mod status;
pub mod upload;

pub(crate) fn be16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

pub(crate) fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
