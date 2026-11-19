//! System1 secure-session layer: the SECURE_REALTIME keepalive and the B-CAS
//! card-session stage, over the encrypted type-8 RPC. (The vendor also runs a "CAS
//! channel", 04/02/02 - 05/02/06 x4 - 06/0c/02, before each stream start; the stream
//! comes the same without it, terrestrial and BS, single-service and all services:
//! hardware 2026-10-29.)
//!
//! Provided by a third party: the byte sequences run on real hardware and the fixed
//! constants match our own extraction byte-for-byte. All
//! RPC bodies are AES-128-CBC encrypted with the boot-auth session key (SS, IV = 0).
//!
//! Encrypted type-8 RPC block: `[0]` group, `[1]` cmd, `[2]` sub, `[10..12]` count (BE),
//! `[12..12+N]` = AES-128-CBC(SS, IV = 0) of the zero-padded plaintext. The reply is
//! read back from 0x083000; its body from offset 12 is encrypted the same way. (A
//! plaintext RPC instead carries 0xFFFFFFFF at `[12..16]` and the payload from `[16]`.)

use tracing::{info, warn};

use crate::auth::{Block, cbc0_decrypt, cbc0_encrypt, cmac, nonce, t8_raw};
use crate::{Error, Result};
use dt295_core::proto::{Io, sleep_ms};

/// KA_KEY = K_RT (xmmword_1400784F8 ^ xmmword_140078538).
const KA_KEY: [u8; 16] = [
    0x45, 0xde, 0xb0, 0x0a, 0x14, 0x65, 0x75, 0xd0, 0x84, 0x2a, 0x7b, 0x20, 0x9b, 0x00, 0xd0, 0x96,
];
/// KA_C2 (28 B) = CMAC1_BLK0 (16) || CMAC1_BLK[0..12].
const KA_C2: [u8; 28] = [
    0xc2, 0x94, 0xa1, 0xbd, 0x41, 0xa9, 0xe3, 0x8e, 0xf3, 0x5b, 0xa1, 0x83, 0x6b, 0x93, 0xda, 0x6c,
    0xd9, 0x5a, 0x1f, 0x24, 0x8b, 0xa9, 0x3d, 0x6b, 0x39, 0xd2, 0xa3, 0x6b,
];

/// Most plaintext one RPC carries.
const MAX_PT: usize = 496;

fn sesskey(io: &Io) -> Result<[u8; 16]> {
    io.p.sesskey
        .ok_or_else(|| Error::State("no secure session key (authenticate first)".into()))
}

/// The block of an encrypted RPC whose plaintext payload is `pt` (zero-padded to a
/// 16-byte multiple before encryption).
pub(crate) fn rpc_block(
    ss: &[u8; 16],
    group: u8,
    cmd: u8,
    sub: u8,
    count: u16,
    pt: &[u8],
) -> Result<Block> {
    if pt.len() > MAX_PT {
        return Err(Error::Param("secure RPC payload longer than 496 bytes"));
    }
    let mut blk = [0u8; 512];
    blk[0] = group;
    blk[1] = cmd;
    blk[2] = sub;
    blk[10..12].copy_from_slice(&count.to_be_bytes());
    if !pt.is_empty() {
        let n = pt.len().div_ceil(16) * 16;
        blk[12..12 + pt.len()].copy_from_slice(pt);
        cbc0_encrypt(ss, &mut blk[12..12 + n]);
    }
    Ok(blk)
}

/// Encrypted type-8 RPC; returns the raw 512-byte reply on transport success (the
/// caller checks the firmware status word `reply[10..12]`).
fn secure_rpc(io: &mut Io, group: u8, cmd: u8, sub: u8, count: u16, pt: &[u8]) -> Result<Block> {
    let ss = sesskey(io)?;
    let blk = rpc_block(&ss, group, cmd, sub, count, pt)?;
    Ok(t8_raw(io, &blk).map_err(Error::into_usb)?.0)
}

/// The plaintext 02/01/02 keepalive challenge (count 0, no payload: 0xFFFFFFFF marker).
fn ka_challenge_block() -> Block {
    let mut blk = [0u8; 512];
    blk[..3].copy_from_slice(&[0x02, 0x01, 0x02]);
    blk[12..16].fill(0xFF);
    blk
}

/// The 32-byte nonce of a challenge reply: its body (from offset 12, trailing zeros
/// trimmed to a 16-byte multiple) CBC(SS)-decrypted holds it at `[4..36]`. `None` if
/// the body is shorter than three blocks.
fn ka_nonce(ss: &[u8; 16], reply: &Block) -> Option<[u8; 32]> {
    let mut body = [0u8; 500];
    body.copy_from_slice(&reply[12..]);
    let mut blen = body.len();
    while blen > 16 && body[blen - 1] == 0 {
        blen -= 1;
    }
    blen = blen / 16 * 16;
    if blen < 48 {
        return None;
    }
    cbc0_decrypt(ss, &mut body[..blen]);
    let mut n = [0u8; 32];
    n.copy_from_slice(&body[4..36]);
    Some(n)
}

/// The 02/02/04 response payload: ff ff ff ff || AES-CMAC(KA_KEY, nonce || KA_C2 ||
/// BE32(seq)) || 2 random bytes || 00 (zero-padded to 496).
fn ka_payload(nonce32: &[u8; 32], seq: u32, rnd: [u8; 2]) -> [u8; MAX_PT] {
    let mut m = [0u8; 64];
    m[..32].copy_from_slice(nonce32);
    m[32..60].copy_from_slice(&KA_C2);
    m[60..].copy_from_slice(&seq.to_be_bytes());
    let mut pt = [0u8; MAX_PT];
    pt[..4].fill(0xFF);
    pt[4..20].copy_from_slice(&cmac(&KA_KEY, &m));
    pt[20..22].copy_from_slice(&rnd);
    pt
}

/// One SECURE_REALTIME keepalive round: the plaintext 02/01/02
/// challenge, answered with 02/02/04 ([`ka_payload`]). Must run regularly (the stream
/// path sends it every 12 s) or the firmware drops state 0x0002 to 0xFFFF.
pub(crate) fn keepalive(io: &mut Io) -> Result<()> {
    let ss = sesskey(io)?;
    let (resp, _) = t8_raw(io, &ka_challenge_block()).map_err(Error::into_usb)?;
    if resp[..3] != [0x02, 0x01, 0x02] {
        warn!(
            "keepalive: bad challenge reply {:02x}{:02x}{:02x}",
            resp[0], resp[1], resp[2]
        );
        return Err(Error::Protocol("keepalive: bad challenge reply".into()));
    }
    let Some(n) = ka_nonce(&ss, &resp) else {
        warn!("keepalive: short challenge");
        return Err(Error::Protocol("keepalive: short challenge".into()));
    };
    io.p.ka_seq = io.p.ka_seq.wrapping_add(1);
    let mut rnd = [0u8; 2];
    nonce(&mut rnd);
    let pt = ka_payload(&n, io.p.ka_seq, rnd);
    let resp = secure_rpc(io, 0x02, 0x02, 0x04, 5, &pt).map_err(Error::into_usb)?;
    if resp[..3] != [0x02, 0x02, 0x04] || (resp[10] | resp[11]) != 0 {
        warn!(
            "keepalive: rejected {:02x}{:02x}{:02x} st={:02x}{:02x}",
            resp[0], resp[1], resp[2], resp[10], resp[11]
        );
        return Err(Error::Protocol("keepalive rejected".into()));
    }
    Ok(())
}

/// One stage of the card session (ch1 doorbell 0a/`stage`/par=1): its ACK.
fn bcas_stage(io: &Io, stage: u8) -> u16 {
    let _ = io.drain_interrupt(50);
    let _ = io.doorbell(1, 0x0A, stage, 0x0001);
    let _ = io.drain_interrupt(300);
    io.read16(0x80016).unwrap_or(0)
}

/// B-CAS stage 2: re-arm the card with stage 1 (ch1 doorbell
/// 0a/00/par=1), then send stage 2 (0a/01/par=1); the ch1 ACK register (0x80016) low
/// byte must be 0 (0x0a00 accept, not 0x0a80 reject). Opens the internal B-CAS card
/// session that un-gates the main ES; needs a contract card in the internal slot.
/// Best effort.
pub(crate) fn bcas_stage2(io: &mut Io) -> Result<()> {
    for _ in 0..6 {
        let s1 = bcas_stage(io, 0x00);
        let r = bcas_stage(io, 0x01);
        info!(
            "b-cas stage1={s1:04x} stage2={r:04x}{}",
            if r & 0xFF != 0 { "  (rejected)" } else { "" }
        );
        if r & 0xFF == 0 {
            return Ok(());
        }
        sleep_ms(500);
    }
    warn!("b-cas stage 2 not accepted (0x_a80); main ES will stay gated (need a contract card)");
    Err(Error::Protocol("b-cas stage 2 not accepted".into()))
}

/// Stage 2 of the card session alone (0a/01), for a card put back in while the
/// session lives: the ch1 ACK's low byte is 0 when accepted.
pub(crate) fn bcas_stage2_only(io: &mut Io) -> Result<()> {
    let r = bcas_stage(io, 0x01);
    info!(
        "b-cas stage2={r:04x}{}",
        if r & 0xFF != 0 { "  (rejected)" } else { "" }
    );
    (r & 0xFF == 0).ok_or_else(|| Error::Protocol("b-cas stage 2 not accepted".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::tests::{SESSKEY, unhex};
    use crate::auth::{CMAC1_BLK, CMAC1_BLK0};

    fn ss() -> [u8; 16] {
        unhex(SESSKEY).try_into().unwrap()
    }

    fn block(prefix: &str) -> Block {
        let mut b = [0u8; 512];
        let p = unhex(prefix);
        b[..p.len()].copy_from_slice(&p);
        b
    }

    #[test]
    fn keepalive_constants_are_the_auth_ones() {
        assert_eq!(KA_C2[..16], CMAC1_BLK0);
        assert_eq!(KA_C2[16..], CMAC1_BLK[..12]);
    }

    #[test]
    fn challenge_block() {
        assert_eq!(
            ka_challenge_block()[..16].to_vec(),
            unhex("020102000000000000000000ffffffff")
        );
        assert!(ka_challenge_block()[16..].iter().all(|&b| b == 0));
    }

    /// The keepalive answer to a model challenge reply (counter 6 -> 7): the whole
    /// 02/02/04 block, with two fixed random bytes.
    #[test]
    fn keepalive_response() {
        let reply = block(
            "020102000000000000000000b9347e9f931165fcf4e01c75704824ccdc788b5702eca71c6c13bed3\
             3cad6e9ef64e0d728126cf87e04ff92decd296aa00000000",
        );
        let n = ka_nonce(&ss(), &reply).unwrap();
        // the model's plaintext: 0xA0 + 3i
        assert_eq!(
            n.to_vec(),
            (4..36).map(|i| (0xA0 + 3 * i) as u8).collect::<Vec<_>>()
        );
        let pt = ka_payload(&n, 7, [0xc2, 0x37]);
        assert_eq!(
            pt[..32].to_vec(),
            unhex("ffffffff699e639fcb7cfa0f17b60edbb7ad3c7fc23700000000000000000000")
        );
        let blk = rpc_block(&ss(), 0x02, 0x02, 0x04, 5, &pt).unwrap();
        let c = unhex(concat!(
            "020204000000000000000005205057def6c421c78389c926764a75d0b78300f597bc03ccdc4577e1",
            "c8c02abf706f70b427d7cb525d64d65bc1c38250a1c3f779fb26b2fd8840b04b46c895bd3d27307b",
            "9ef7a804fe12af95e5ac3bf5cbdc5c07b08bd01d6a61875abfae6aff5860d18ea211a6c50a5e3d9f",
            "ae3940bb8bbe29bbaf10eb9801a9ae662c57cc88f25ba98623609568aad752a16ff3a4685e516f7f",
            "f82489356b1d3d364a24e3eac6627a67d84ed9d3194ea974b513f549ac48722b0f34f5d50ce28ac3",
            "1a9771bdcafa142b928b9ee19e2218e0169da8646148b22d517d828e4828d05361a9ab06c4047fde",
            "24190a3cf567684d659cd1bfc17159d7b364a08c4a18e088837ef4a9d74192eef868a5b89829c994",
            "bafd60e69f302c338e9bb3d8b86376cdf36e3e3a0c234e4daa8653566956b71a5259f9bf0a6f0329",
            "840dd365614b633a8f11d13c23b464ef7c329150ef60e06e57ec4ee1860a379cbcbfaebed132f1c0",
            "ec3fe642acd2a3ca1c7cc2fb24309d4e8aa6a067d446fe5ed5b88d2a9629eaf5e933195c36dca722",
            "f88008cfc580c0b16ab42533f367529e6589301814fcdcfce83be6f24a69fdaf8583487767098c55",
            "9c23bc265aa180537fa1b3af3175ab4bdb65d849352a3620b049a96ba0cca890206fb990a3d24483",
            "dcaafdbced46ac490047a9d62f813af15b75066b9846daf20e1e145b00000000",
        ));
        assert_eq!(blk.to_vec(), c);
    }

    #[test]
    fn short_challenge_is_rejected() {
        let reply = block("02010200000000000000000011223344556677889900aabbccddeeff");
        assert!(ka_nonce(&ss(), &reply).is_none());
    }

    #[test]
    fn rpc_rejects_oversized_payload() {
        assert!(rpc_block(&ss(), 1, 2, 3, 0, &[0u8; 497]).is_err());
        assert!(rpc_block(&ss(), 1, 2, 3, 0, &[0u8; 496]).is_ok());
    }
}
