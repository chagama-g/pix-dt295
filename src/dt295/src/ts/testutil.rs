//! Builders shared by the `ts` unit tests.

use aes::Aes128;
use aes::cipher::{Block, BlockCipherEncrypt, BlockModeEncrypt, KeyInit, KeyIvInit};

use super::crc;
use super::link::FIXED_IV;
use super::packet::{self, PACKET, SYNC};

/// One PID-0 packet carrying one PAT section of TS 0x7FE8 (afc 1, PUSI, pointer_field
/// `ptr`); `entries` are (program_number, PMT PID). Port of `make_pat`.
pub fn make_pat_packet(entries: &[(u16, u16)], ptr: usize, corrupt_crc: bool) -> [u8; PACKET] {
    let mut q = [0xFFu8; PACKET];
    q[..4].copy_from_slice(&[SYNC, 0x40, 0x00, 0x10]);
    q[4] = ptr as u8;
    let o = 5 + ptr;
    let sl = 5 + 4 * entries.len() + 4;
    q[o..o + 8].copy_from_slice(&[
        0x00,
        0xB0 | (sl >> 8) as u8,
        sl as u8,
        0x7F,
        0xE8,
        0xC1,
        0,
        0,
    ]);
    let mut p = o + 8;
    for &(prog, pid) in entries {
        q[p..p + 2].copy_from_slice(&prog.to_be_bytes());
        q[p + 2..p + 4].copy_from_slice(&(0xE000 | pid).to_be_bytes());
        p += 4;
    }
    let mut crc = crc::crc32(&q[o..p]);
    if corrupt_crc {
        crc ^= 1;
    }
    q[p..p + 4].copy_from_slice(&crc.to_be_bytes());
    q
}

/// Packetize one section on `pid` (continuity counter from `*cc`, counting up), the
/// first packet with `ptr_pad` bytes skipped by its pointer_field. Port of `packetize`.
pub fn packetize(pid: u16, sec: &[u8], cc: &mut u8, ptr_pad: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut off = 0;
    let mut first = true;
    while off < sec.len() {
        let start = out.len();
        let pusi = if first { 0x40 } else { 0x00 };
        out.extend_from_slice(&[
            SYNC,
            pusi | (pid >> 8) as u8,
            pid as u8,
            0x10 | (*cc & 0x0F),
        ]);
        *cc = cc.wrapping_add(1);
        if first {
            out.push(ptr_pad as u8);
            out.resize(out.len() + ptr_pad, 0xAA);
        }
        let take = (sec.len() - off).min(start + PACKET - out.len());
        out.extend_from_slice(&sec[off..off + take]);
        out.resize(start + PACKET, 0xFF);
        off += take;
        first = false;
    }
    out
}

/// The device's link encryption of `len` bytes: AES-128-CBC over the whole blocks,
/// then the tail XORed with the encrypted last ciphertext block (or the IV). Port of
/// the dtvdec selftest's `region_encrypt`.
pub fn region_encrypt(key: &[u8; 16], buf: &mut [u8], iv: &[u8; 16]) {
    let n = buf.len() / 16 * 16;
    let (body, tail) = buf.split_at_mut(n);
    let (blocks, _) = Block::<Aes128>::slice_as_chunks_mut(body);
    cbc::Encryptor::<Aes128>::new(key.into(), iv.into()).encrypt_blocks(blocks);
    if !tail.is_empty() {
        let prev: [u8; 16] = match body.last_chunk::<16>() {
            Some(last) => *last,
            None => *iv,
        };
        let mut ks = Block::<Aes128>::from(prev);
        Aes128::new(key.into()).encrypt_block(&mut ks);
        tail.iter_mut().zip(ks.iter()).for_each(|(b, k)| *b ^= k);
    }
}

/// Link-encrypt a packet's payload the way the device does and set TSC = 01.
pub fn link_encrypt(key: &[u8; 16], p: &mut [u8]) {
    if let Some(s) = packet::payload_offset(p) {
        region_encrypt(key, &mut p[s..PACKET], &FIXED_IV);
    }
    p[3] = (p[3] & 0x3F) | 0x40;
}

/// A tiny deterministic PRNG for the no-panic sweeps (xorshift64*).
#[derive(Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 32) as u8
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        buf.iter_mut().for_each(|b| *b = self.byte());
    }
}
