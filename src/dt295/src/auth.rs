//! MB86M01 firmware authentication handshake and the type-8 message transport.
//!
//! Reproduces cmd_auth_Initial / cmd_auth_ComputeResponse / cmd_auth_DeriveSessionKey
//! of the vendor driver. Two type-8 messages:
//! * MSG1 (cmd 0x0101): send the 24-byte identity header, receive a challenge.
//! * MSG2 (cmd 0x0102): send a 20-byte response (AES-CMAC over a key-table entry
//!   selected by an index derived from the challenge),
//!
//! then derive the 16-byte session key. All key material and constants were recovered
//! from the driver image (docs/device.md, authentication).

use aes::Aes128;
use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use tracing::{debug, info, warn};

use dt295_core::proto::{Io, REG_IRQ, sleep_ms};
use crate::{Error, Result};

// ---- AES-128 helpers ----

pub(crate) fn aes(key: &[u8; 16]) -> Aes128 {
    Aes128::new(&Array::from(*key))
}

pub(crate) fn encrypt_block(c: &Aes128, b: &mut [u8; 16]) {
    let mut blk = Array::from(*b);
    c.encrypt_block(&mut blk);
    *b = blk.into();
}

pub(crate) fn decrypt_block(c: &Aes128, b: &mut [u8; 16]) {
    let mut blk = Array::from(*b);
    c.decrypt_block(&mut blk);
    *b = blk.into();
}

/// AES-128-CBC encrypt with IV = 0, in place, over the whole 16-byte blocks of `buf`.
pub(crate) fn cbc0_encrypt(key: &[u8; 16], buf: &mut [u8]) {
    let c = aes(key);
    let mut chain = [0u8; 16];
    for blk in buf.as_chunks_mut::<16>().0 {
        for (x, y) in chain.iter_mut().zip(blk.iter()) {
            *x ^= y;
        }
        encrypt_block(&c, &mut chain);
        blk.copy_from_slice(&chain);
    }
}

/// AES-128-CBC decrypt with IV = 0, in place, over the whole 16-byte blocks of `buf`.
pub(crate) fn cbc0_decrypt(key: &[u8; 16], buf: &mut [u8]) {
    let c = aes(key);
    let mut chain = [0u8; 16];
    for blk in buf.as_chunks_mut::<16>().0 {
        let mut b = [0u8; 16];
        b.copy_from_slice(blk);
        let next = b;
        decrypt_block(&c, &mut b);
        for ((o, x), y) in blk.iter_mut().zip(b).zip(chain) {
            *o = x ^ y;
        }
        chain = next;
    }
}

/// Fresh random bytes for protocol nonces. The device anti-replays them: a repeated
/// nonce makes the firmware return stale data -- e.g. a wrong 07/01/06 link key, which
/// left the TS undecryptable on all but the first session after a power-cycle.
pub(crate) fn nonce(out: &mut [u8]) {
    if getrandom::fill(out).is_ok() {
        return;
    }
    // The OS generator failed (never seen): an xorshift64, seeded from the
    // clock and a per-process counter. Not cryptographic, but it never repeats a nonce
    // across sessions, which is all the firmware requires.
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut s = t
        .wrapping_mul(2_862_933_555_777_941_757)
        .wrapping_add(3_037_000_493)
        ^ (CALLS.fetch_add(1, Ordering::Relaxed) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ u64::from(std::process::id()) << 32;
    if s == 0 {
        s = 88_172_645_463_325_252;
    }
    for b in out {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *b = (s >> 33) as u8;
    }
}

// ---- recovered constants (raw little-endian bytes as stored in the image) ----

/// AES-CMAC keys = xmmword_1400784C8/D8/E8 XOR xmmword_140078508/518/528.
const K_AUTH1: [u8; 16] = [
    0x46, 0x1b, 0x25, 0x35, 0x6b, 0xae, 0xa3, 0xf8, 0x25, 0x25, 0xf6, 0x33, 0xef, 0x71, 0x12, 0xaf,
];
const K_AUTH2: [u8; 16] = [
    0x7e, 0xf1, 0x92, 0x24, 0x8e, 0x1c, 0xd3, 0xc4, 0x75, 0x50, 0xdb, 0x1f, 0xb7, 0x6d, 0x24, 0x50,
];
const K_SESS: [u8; 16] = [
    0xd5, 0x3c, 0xfb, 0xe7, 0x0d, 0x21, 0x07, 0x2c, 0xd3, 0xc0, 0xcc, 0xe3, 0xf9, 0x88, 0xaa, 0x6d,
];

/// CMAC1 block 0 = xmmword_140078478 (constant).
pub(crate) const CMAC1_BLK0: [u8; 16] = [
    0xc2, 0x94, 0xa1, 0xbd, 0x41, 0xa9, 0xe3, 0x8e, 0xf3, 0x5b, 0xa1, 0x83, 0x6b, 0x93, 0xda, 0x6c,
];
/// CMAC1 block 1 template: qword_140078488, dword_140078490, word_140078494 (bytes 14
/// and 15 are replaced by the challenge byte and the seed byte at runtime).
pub(crate) const CMAC1_BLK: [u8; 16] = [
    0xd9, 0x5a, 0x1f, 0x24, 0x8b, 0xa9, 0x3d, 0x6b, 0x39, 0xd2, 0xa3, 0x6b, 0x40, 0x59, 0x00, 0x00,
];
/// CMAC3 trailing constants: qword_140078498 (8) + dword_1400784A0 (4).
const CMAC3_TAIL: [u8; 12] = [
    0x95, 0x87, 0xd2, 0xe8, 0x90, 0xcb, 0x4e, 0x5c, 0x70, 0x91, 0xb1, 0xf9,
];

/// 16 x 32-byte key table at 0x140077920.
#[rustfmt::skip]
const KEYTABLE: [[u8; 32]; 16] = [
    [
        0xe2, 0x94, 0xba, 0x28, 0x3b, 0x49, 0x03, 0x7e, 0xfe, 0xdc, 0xe2, 0xbe, 0xd8, 0x50, 0xe4, 0xc7,
        0xc2, 0xb4, 0x1f, 0x89, 0x42, 0xac, 0xa5, 0x1b, 0x82, 0xb4, 0x7b, 0xab, 0x75, 0x0c, 0x6a, 0x46,
    ],
    [
        0xc0, 0xce, 0x93, 0x69, 0xd9, 0xcc, 0xae, 0xd5, 0xfe, 0x37, 0xdf, 0x76, 0xf6, 0x50, 0x64, 0xfd,
        0xb0, 0x2f, 0xed, 0x21, 0x6e, 0x4c, 0x77, 0xbd, 0xca, 0xe7, 0x02, 0xfa, 0x23, 0xcc, 0x0f, 0x63,
    ],
    [
        0x50, 0x8d, 0x8e, 0xe3, 0x4c, 0x11, 0x4e, 0xb8, 0x06, 0x2e, 0xb3, 0x3b, 0xd0, 0xed, 0x15, 0x39,
        0x28, 0x8d, 0x7b, 0x71, 0x3f, 0xd0, 0xc5, 0x9c, 0xd3, 0x2f, 0x6b, 0x23, 0xc3, 0x20, 0x36, 0x88,
    ],
    [
        0x3a, 0x90, 0xa5, 0xb4, 0x40, 0x47, 0x2c, 0x4f, 0x39, 0x45, 0x93, 0x2b, 0xd8, 0xcc, 0x00, 0xee,
        0xc6, 0x26, 0x0a, 0x5b, 0xc2, 0x35, 0x2f, 0xa0, 0x35, 0xe5, 0x44, 0xfb, 0x6a, 0x76, 0x40, 0xaf,
    ],
    [
        0x7a, 0xb2, 0xd5, 0x82, 0xb4, 0x1e, 0x21, 0xf9, 0xd8, 0x63, 0x64, 0xab, 0x21, 0x38, 0x7c, 0x5b,
        0xf6, 0xc8, 0x81, 0x58, 0xb3, 0x12, 0x67, 0x43, 0xff, 0x04, 0xd0, 0x70, 0xfa, 0x76, 0x4c, 0xa6,
    ],
    [
        0x95, 0xa7, 0x11, 0xc7, 0x3f, 0x38, 0xbd, 0xce, 0x3b, 0xda, 0xff, 0x0f, 0xf9, 0x4c, 0xf5, 0xbb,
        0xd9, 0xf5, 0x03, 0x1f, 0x25, 0x9a, 0xf6, 0x2b, 0x73, 0xa0, 0x10, 0x05, 0xca, 0x0b, 0x75, 0x2a,
    ],
    [
        0x3b, 0x0c, 0x76, 0x5b, 0xb0, 0xc4, 0x00, 0x9d, 0x1e, 0x0e, 0x05, 0x2b, 0x21, 0xad, 0x57, 0xa3,
        0x9f, 0xef, 0x43, 0xcc, 0x86, 0x7b, 0xc8, 0x8f, 0xf5, 0x6b, 0x1e, 0x04, 0x05, 0xd8, 0x49, 0x37,
    ],
    [
        0xbc, 0x09, 0x51, 0x04, 0xb6, 0xc4, 0x3a, 0xfa, 0xbc, 0xbc, 0xd7, 0x21, 0x90, 0x50, 0xf1, 0x12,
        0x94, 0x0e, 0xa5, 0x8a, 0x63, 0xff, 0xb4, 0x57, 0xa5, 0x93, 0x97, 0xaf, 0x3e, 0x56, 0xb7, 0x08,
    ],
    [
        0x29, 0xeb, 0x69, 0xe4, 0xe8, 0xaf, 0x61, 0x0b, 0x79, 0x03, 0xfb, 0xf7, 0xba, 0x86, 0x8d, 0x43,
        0x6e, 0x22, 0xe5, 0xea, 0x98, 0xa4, 0x59, 0x38, 0x17, 0x25, 0x62, 0x56, 0xb1, 0x09, 0x8b, 0xed,
    ],
    [
        0x6e, 0x78, 0x91, 0x3d, 0x7c, 0xb3, 0x59, 0xbc, 0xf0, 0x51, 0xed, 0xe1, 0x38, 0x06, 0xbb, 0x4c,
        0x9b, 0x5f, 0x9f, 0xd8, 0x02, 0xfd, 0x73, 0x9d, 0x6f, 0xd3, 0x0a, 0x5f, 0xba, 0xc8, 0x6a, 0xee,
    ],
    [
        0xe8, 0xfa, 0xa5, 0x1c, 0x89, 0x0b, 0x6b, 0x87, 0xdd, 0xd2, 0x1b, 0x01, 0x25, 0x06, 0x5c, 0xc7,
        0x76, 0x12, 0x25, 0xc6, 0xd6, 0xc8, 0xad, 0xd2, 0x53, 0x52, 0x99, 0x6b, 0x7c, 0x50, 0xd0, 0x98,
    ],
    [
        0x24, 0xe0, 0xf2, 0xa5, 0xd8, 0x1c, 0xe8, 0x76, 0x30, 0x4a, 0xb6, 0x12, 0xf7, 0x1d, 0x12, 0x4d,
        0xd0, 0x99, 0x33, 0xfe, 0xb4, 0x3c, 0xad, 0xa5, 0x87, 0x46, 0xca, 0x25, 0xe2, 0x26, 0x67, 0xc8,
    ],
    [
        0xdc, 0xf0, 0xe1, 0x06, 0xd9, 0xe9, 0x25, 0x42, 0x9e, 0x4f, 0xe2, 0xde, 0x13, 0x3e, 0x2c, 0x57,
        0x0d, 0x00, 0xa6, 0x68, 0x40, 0x0a, 0x8b, 0x78, 0xf0, 0xae, 0x33, 0xd6, 0x9d, 0xb1, 0xae, 0x06,
    ],
    [
        0x64, 0x86, 0x17, 0x8a, 0x62, 0xa8, 0x32, 0x1d, 0x2c, 0x3c, 0x0f, 0x35, 0xa0, 0xa2, 0x70, 0x53,
        0x09, 0xd3, 0x6b, 0x5e, 0x8a, 0x24, 0xea, 0xc6, 0x18, 0x83, 0xd0, 0xa7, 0x85, 0x0f, 0x10, 0x10,
    ],
    [
        0x4d, 0xcd, 0x6b, 0x0b, 0x57, 0xa7, 0xf6, 0xc2, 0x44, 0x95, 0x24, 0x60, 0x8c, 0x84, 0xed, 0xd2,
        0xde, 0x2a, 0xed, 0x3a, 0x27, 0x9b, 0xb5, 0x9b, 0x9c, 0x4b, 0x9c, 0x5e, 0x44, 0xc9, 0xe5, 0x85,
    ],
    [
        0xbb, 0x2d, 0x2e, 0x3a, 0xfd, 0xe5, 0x24, 0x3d, 0x20, 0x93, 0xc5, 0x15, 0x7e, 0x4b, 0xf3, 0xa8,
        0x28, 0x81, 0xa8, 0xc5, 0xdf, 0xd2, 0x1b, 0xfd, 0xcc, 0xcb, 0x4e, 0x43, 0x15, 0xa7, 0x6e, 0xe6,
    ],
];

/// Identity header {2018, 5, 1, 1040, 1, 6} as little-endian dwords (dword_14008A068).
const ID_HEADER: [u32; 6] = [2018, 5, 1, 1040, 1, 6];

/// The challenge byte position in the MSG1 reply and the CMAC1 seed byte (the values
/// the firmware accepts, verified on hardware).
const CHAL_POS: usize = 48;
const SEED: u8 = 0xE2;

// ---- the driver's CMAC ----

fn dbl128(i: &[u8; 16]) -> [u8; 16] {
    let mut o = [0u8; 16];
    for k in 0..15 {
        o[k] = (i[k] << 1) | (i[k + 1] >> 7);
    }
    o[15] = i[15] << 1;
    if i[0] & 0x80 != 0 {
        o[15] ^= 0x87;
    }
    o
}

/// The driver's AES-128-CMAC (`quirk_cmac`). The vendor loop encrypts each raw block
/// with CNG AES-CBC; because CNG retains the CBC state across calls, that is
/// mathematically the standard CMAC (verified against the NIST SP 800-38B vectors).
/// All auth inputs are multiples of 16, so only the
/// K1 (complete-block) path is exercised; K2 padding is handled for safety.
pub(crate) fn cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let c = aes(key);
    let mut l = [0u8; 16];
    encrypt_block(&c, &mut l);
    let k1 = dbl128(&l);
    let k2 = dbl128(&k1);
    let nb = data.len().div_ceil(16).max(1);
    let r = data.len() % 16;
    let mut x = [0u8; 16];
    for blk in data.chunks(16).take(nb - 1) {
        for (a, b) in x.iter_mut().zip(blk) {
            *a ^= b;
        }
        encrypt_block(&c, &mut x);
    }
    let last = &data[(nb - 1) * 16..];
    let mut blk = [0u8; 16];
    if r == 0 && !data.is_empty() {
        blk.copy_from_slice(last);
        for (a, b) in blk.iter_mut().zip(k1) {
            *a ^= b;
        }
    } else {
        blk[..r].copy_from_slice(last);
        blk[r] = 0x80;
        for (a, b) in blk.iter_mut().zip(k2) {
            *a ^= b;
        }
    }
    for (a, b) in x.iter_mut().zip(blk) {
        *a ^= b;
    }
    encrypt_block(&c, &mut x);
    x
}

// ---- type-8 message transport ----
//
// The firmware writes the response back in place at the parameter region 0x083000
// (cmd_ReadAckRegister reads 512 B from 0x083000 when the ack type is 8) and signals
// completion via the ch0 ACK register 0x080014 (high byte = ack type, low byte =
// result). The 0x083100 "secure response" path is only for async secure / transcode
// responses, not the auth handshake.

const T8_ADDR: u32 = 0x083000;
const T8_ACK_REG: u32 = 0x080014;

/// A type-8 block: the 512-byte parameter / reply window.
pub(crate) type Block = [u8; 512];

/// The 512-byte parameter block of a plaintext message (device big-endian words):
/// `[cmd, 0, 0, 0, 0, len_dwords, 0xFFFF, 0xFFFF, payload...]`. `ndw` = the declared
/// payload length in dwords (the vendor's length field); the payload is copied as is
/// (each source dword stored big-endian) and zero-padded to `ndw` dwords.
fn build_param(cmd: u16, payload: &[u8], ndw: usize) -> Block {
    let mut b = [0u8; 512];
    let hdr: [u16; 8] = [cmd, 0, 0, 0, 0, ndw as u16, 0xFFFF, 0xFFFF];
    for (i, w) in hdr.iter().enumerate() {
        b[2 * i..2 * i + 2].copy_from_slice(&w.to_be_bytes());
    }
    let n = payload.len().min(4 * ndw);
    b[16..16 + n].copy_from_slice(&payload[..n]);
    b
}

/// `dump_hex`: "tag: xx xx ..." within a 160-character line. DEBUG, not
/// INFO: every type-8 exchange (each keepalive, each CAS step) dumped these, and the
/// session key does not belong in a log file at the default level.
fn dump_hex(tag: &str, b: &[u8]) {
    let mut line = format!("{tag}:");
    for x in b {
        if line.len() >= 156 {
            break;
        }
        line.push_str(&format!(" {x:02x}"));
    }
    debug!("{line}");
}

/// Send a pre-built 512-byte type-8 block (doorbell {8, 0} on ch0) and read back the
/// 512-byte reply and its firmware status (reply words 2..3).
pub(crate) fn t8_raw(io: &mut Io, param: &Block) -> Result<(Block, u32)> {
    io.write_block(T8_ADDR, param).map_err(Error::into_usb)?;
    dump_hex("t8 param[0:48]", &param[..48]);
    // clear stale ack/state bits, then doorbell {type=8, sub=0} on ch0
    let _ = io.write_field(REG_IRQ, 3, 1, 1); // 0x08 state
    let _ = io.write_field(REG_IRQ, 1, 1, 1); // 0x02 generic ack
    match io.control_out(0xB8, 0, 0, &[8, 0, 0, 0]) {
        Ok(4) => {}
        Ok(n) => {
            return Err(Error::Transfer(format!(
                "type-8 doorbell: short write ({n})"
            )));
        }
        Err(e) => return Err(e.into_usb()),
    }
    // poll the ch0 ACK register: high byte = ack type (expect 8), low = result
    let (mut s, mut ack, mut waited) = (0u16, 0u16, 0u32);
    loop {
        let _ = io.drain_interrupt(50);
        io.read16_into(REG_IRQ, &mut s);
        if io.read16_into(T8_ACK_REG, &mut ack) && (ack >> 8) != 0 {
            break;
        }
        if s & (0x08 | 0x02) != 0 {
            break;
        }
        if waited >= io.p.cmd_timeout_ms {
            warn!("t8: no ack (status 0x{s:04x} ack 0x{ack:04x})");
            return Err(Error::Timeout("type-8 ack"));
        }
        sleep_ms(5);
        waited += 5;
    }
    debug!(
        "t8: ack_reg=0x{ack:04x} (type={} result={}) status=0x{s:04x}",
        ack >> 8,
        ack & 0xFF
    );
    let mut resp = [0u8; 512];
    io.read_block(T8_ADDR, &mut resp).map_err(Error::into_usb)?;
    let _ = io.write_field(REG_IRQ, 3, 1, 1); // W1C state
    let _ = io.write_field(REG_IRQ, 1, 1, 1); // W1C generic
    // word[0..1] = echo, word[2..3] = status
    let w = |i: usize| u16::from_be_bytes([resp[2 * i], resp[2 * i + 1]]);
    debug!(
        "t8 resp echo={:04x}{:04x} status={:04x}{:04x}",
        w(0),
        w(1),
        w(2),
        w(3)
    );
    let status = (u32::from(w(2)) << 16) | u32::from(w(3));
    dump_hex("t8 resp[0:64]", &resp[..64]);
    Ok((resp, status))
}

/// A plaintext type-8 message ([`build_param`]) and its reply / status.
fn t8_exchange(io: &mut Io, cmd: u16, payload: &[u8], ndw: usize) -> Result<(Block, u32)> {
    t8_raw(io, &build_param(cmd, payload, ndw))
}

fn id_header() -> [u8; 24] {
    let mut h = [0u8; 24];
    for (i, v) in ID_HEADER.iter().enumerate() {
        h[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }
    h
}

/// Key-table index for a challenge byte: CMAC1 over block0 || block1(chal, seed);
/// `idx = mac[15] & 0xF`.
fn keytable_index(chal: u8, seed: u8) -> usize {
    let mut d = [0u8; 32];
    d[..16].copy_from_slice(&CMAC1_BLK0);
    d[16..].copy_from_slice(&CMAC1_BLK);
    d[16 + 14] = chal;
    d[16 + 15] = seed;
    usize::from(cmac(&K_AUTH1, &d)[15] & 0x0F)
}

/// The MSG2 response to a MSG1 reply: CMAC2 = CMAC(K_AUTH2, challenge payload
/// reply[16..48] || kt[idx]) followed by the fresh nonce (2 random + 2 zero bytes, as
/// provided by a third party). Returns (idx, response).
fn auth_response(msg1: &Block, chal_pos: usize, seed: u8, nonce2: [u8; 2]) -> (usize, [u8; 20]) {
    let idx = keytable_index(msg1[chal_pos], seed);
    let mut d = [0u8; 64];
    d[..32].copy_from_slice(&msg1[16..48]);
    d[32..].copy_from_slice(&KEYTABLE[idx]);
    let mut r = [0u8; 20];
    r[..16].copy_from_slice(&cmac(&K_AUTH2, &d));
    r[16..18].copy_from_slice(&nonce2);
    (idx, r)
}

/// The session key after an accepted MSG2. cmd_auth_DeriveSessionKey does not store the
/// CMAC as is: it packs key = mac[12..15] | mac[8..11] | mac[0..7] (dwords 3, 2, 0, 1;
/// the disassembly at 0x140048e8e) before cmd_SetSessionKey copies it to LSI+8176, the
/// key cmd_AesCbcEnc/DecSessionKey use.
fn session_key(response: &[u8; 20], idx: usize) -> [u8; 16] {
    let mut d = [0u8; 64];
    d[..20].copy_from_slice(response);
    d[20..52].copy_from_slice(&KEYTABLE[idx]);
    d[52..].copy_from_slice(&CMAC3_TAIL);
    let mac = cmac(&K_SESS, &d);
    let mut k = [0u8; 16];
    k[..4].copy_from_slice(&mac[12..16]);
    k[4..8].copy_from_slice(&mac[8..12]);
    k[8..].copy_from_slice(&mac[..8]);
    k
}

/// One handshake (challenge byte at `chal_pos`, CMAC1 seed `seed`). `Err(Protocol)` if
/// the firmware rejected the response.
fn auth_attempt(io: &mut Io, chal_pos: usize, seed: u8) -> Result<(usize, [u8; 16])> {
    let (resp, status) = t8_exchange(io, 0x0101, &id_header(), 8).map_err(Error::into_usb)?;
    if !(resp[0] == 0x01 && resp[1] == 0x01) || status != 0 {
        return Err(Error::Protocol(format!(
            "auth: bad challenge reply (status 0x{status:x})"
        )));
    }
    let mut n = [0u8; 2];
    nonce(&mut n);
    let (idx, response) = auth_response(&resp, chal_pos, seed, n);
    let (_, status) = t8_exchange(io, 0x0102, &response, 5).map_err(Error::into_usb)?;
    if status != 0 {
        info!(
            "auth: pos={chal_pos} seed=0x{seed:02x} chal=0x{:02x} idx={idx} -> rejected \
             (0x{status:x})",
            resp[chal_pos]
        );
        return Err(Error::Protocol(format!(
            "auth: response rejected (0x{status:x})"
        )));
    }
    Ok((idx, session_key(&response, idx)))
}

/// The boot authentication handshake; stores the session key on success.
pub(crate) fn perform(io: &mut Io) -> Result<()> {
    let (idx, key) = auth_attempt(io, CHAL_POS, SEED)?;
    info!("authentication succeeded (idx={idx})");
    dump_hex("session key", &key);
    io.p.sesskey = Some(key);
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn arr16(s: &str) -> [u8; 16] {
        unhex(s).try_into().unwrap()
    }

    /// A device model's MSG1 reply: bytes 16..64 = i * 7 + 3.
    fn msg1_reply() -> Block {
        let mut b = [0u8; 512];
        b[0] = 1;
        b[1] = 1;
        for (i, x) in b.iter_mut().enumerate().take(64).skip(16) {
            *x = (i * 7 + 3) as u8;
        }
        b
    }

    /// The session key the handshake derives against that model (known answer).
    pub(crate) const SESSKEY: &str = "a0ccf2b656203b858e6be051bfcb042f";

    #[test]
    fn cmac_rfc4493() {
        let k = arr16("2b7e151628aed2a6abf7158809cf4f3c");
        let m = unhex(
            "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710",
        );
        assert_eq!(cmac(&k, &[]), arr16("bb1d6929e95937287fa37d129b756746"));
        assert_eq!(
            cmac(&k, &m[..16]),
            arr16("070a16b46b4d4144f79bdd9dd04a287c")
        );
        assert_eq!(
            cmac(&k, &m[..40]),
            arr16("dfa66747de9ae63030ca32611497c827")
        );
        assert_eq!(cmac(&k, &m), arr16("51f0bebf7e3b9d92fc49741779363cfe"));
    }

    /// The CMAC of fixed inputs (data[i] = i * 13 + 1) under every key: known answers.
    #[test]
    fn cmac_known_answers() {
        let data: Vec<u8> = (0..64u32).map(|i| (i * 13 + 1) as u8).collect();
        let want: [(usize, &str, &str, &str); 10] = [
            (
                0,
                "776b7a9344f8a8f3389d98f904d30ef5",
                "43b000fc0f8191a0bffe24e4e3c5932e",
                "2337ea67cccb5b8570fd5ddb65d17795",
            ),
            (
                1,
                "dc4d5fc1505594dda3752fdf7d567be9",
                "69ffd6ddb564357311afbd56fc415acd",
                "9e81506e8949853bd0613657ef6fe9b5",
            ),
            (
                15,
                "502538fa0f325e3d7533857e30c59611",
                "0c2634207d7dbd28ec081ff904c1d97a",
                "bdeb9e6f24e11d3e25d07ad2c892051c",
            ),
            (
                16,
                "00124ca019678130ab7e96559a91abb5",
                "79b2c63290f3722cb2ac13311539c836",
                "d637c6fdcaf08cc42ff94e4558eef693",
            ),
            (
                17,
                "ed818d2a700ead10a3caaa99a8ec9fc8",
                "70f86a45bf2367bc1269b2c3a79a9d62",
                "579fd296b290115e216873c60aad9482",
            ),
            (
                20,
                "e306c6914a32349b3801d5199c02edb8",
                "3b63e0093d28e78ba5506881869e9a8d",
                "2e177b0b280827123b36da4d06279de5",
            ),
            (
                32,
                "f96d9e7e5f0c5b60a99ed64dd7aeb099",
                "32ab3a69555ee6e9057943157a0c9313",
                "2763705e4a559ac9faccbc91c1332a14",
            ),
            (
                40,
                "560d9b816361461495deb10f30aa67d9",
                "c95d5f79cbe7e8276e1549f315edffd6",
                "2026ae876df6b3f3c00870e16febae7d",
            ),
            (
                63,
                "5f559959978d5bb8114c7357bcf5d157",
                "3a1a9d7f7f80d4ccb8b26525c83f2c14",
                "d8aaa297f3c0185155840d082def843c",
            ),
            (
                64,
                "a25f7a495d14f4dd4549b150667804d2",
                "8b62d18b61a22d8b81d457ad6438026f",
                "549aaa96b2a0e990d9648c30c7b7ebbe",
            ),
        ];
        for (len, a1, a2, ss) in want {
            assert_eq!(cmac(&K_AUTH1, &data[..len]), arr16(a1), "K_AUTH1 len {len}");
            assert_eq!(cmac(&K_AUTH2, &data[..len]), arr16(a2), "K_AUTH2 len {len}");
            assert_eq!(cmac(&K_SESS, &data[..len]), arr16(ss), "K_SESS len {len}");
        }
    }

    /// The key-table index for every challenge byte (seed 0xE2): known answers.
    #[test]
    fn keytable_selection() {
        let c = "5d24c3cf89976ab813e976407ede5cb5758d9ea62da501319ad83c27ab367851\
                 14875ddca87f1e641e06dc0015f60f1f3bc3e851fe424f938cc2bb047b946a14\
                 781b7c1b307db2df4fe7ca3dd68b84f10c9676d02bcfa31dd076d104eb2a2b3b\
                 f014dc7541b5953b3cfc923268fd794ee5bda676f3f8f9e9662967503eeed8d8";
        for (chal, ch) in c.chars().enumerate() {
            let want = ch.to_digit(16).unwrap() as usize;
            assert_eq!(keytable_index(chal as u8, SEED), want, "chal 0x{chal:02x}");
        }
    }

    /// The full handshake against the device model: the MSG2 block (with a fixed
    /// nonce) and the derived session key.
    #[test]
    fn handshake_known_answer() {
        let msg1 = msg1_reply();
        assert_eq!(
            msg1[..64].to_vec(),
            unhex(
                "01010000000000000000000000000000737a81888f969da4abb2b9c0c7ced5dc\
                 e3eaf1f8ff060d141b222930373e454c535a61686f767d848b9299a0a7aeb5bc"
            )
        );
        let c_msg2 = unhex(
            "010200000000000000000005ffffffff922599cf9ff2f096f8c2af083f577d8a\
             a3520000000000000000000000000000",
        );
        let nonce2 = [c_msg2[32], c_msg2[33]];
        let (idx, response) = auth_response(&msg1, CHAL_POS, SEED, nonce2);
        assert_eq!(idx, keytable_index(msg1[CHAL_POS], SEED));
        assert_eq!(build_param(0x0102, &response, 5)[..48].to_vec(), c_msg2);
        assert_eq!(session_key(&response, idx), arr16(SESSKEY));
    }

    #[test]
    fn identity_header() {
        assert_eq!(
            build_param(0x0101, &id_header(), 8)[..40].to_vec(),
            unhex(
                "010100000000000000000008ffffffffe20700000500000001000000100400000100000006000000"
            )
        );
    }

    #[test]
    fn cbc0_roundtrip_and_nist() {
        // SP 800-38A F.2.1 with IV = 0 is not a published vector; check the first block
        // (CBC with a zero IV is plain AES on block 0) and the round trip.
        let k = arr16("2b7e151628aed2a6abf7158809cf4f3c");
        let mut b = unhex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        let orig = b.clone();
        cbc0_encrypt(&k, &mut b);
        assert_eq!(b[..16].to_vec(), unhex("3ad77bb40d7a3660a89ecaf32466ef97")); // ECB F.1.1
        cbc0_decrypt(&k, &mut b);
        assert_eq!(b, orig);
    }

    #[test]
    fn nonce_fills() {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        nonce(&mut a);
        nonce(&mut b);
        assert_ne!(a, b);
    }
}
