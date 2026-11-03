//! The device's link-layer encryption of EP 0x81 and the link-key check.
//! The payload of each packet with transport_scrambling_control
//! = 01 is AES-128-CBC over its whole 16-byte blocks and a CFB-style tail (the last
//! ciphertext block encrypted, XORed into the remaining bytes), under a per-session
//! key and a device-fixed IV. The key derivation (a firmware RPC) is in the core.
//!
//! This layer sits on top of (and is unrelated to) the ARIB MULTI-2 broadcast
//! scrambling; the TS header and adaptation field are never encrypted.

use aes::Aes128;
use aes::cipher::{Block, BlockCipherEncrypt, BlockModeDecrypt, InnerIvInit, KeyInit};

use super::packet::{self, PACKET, SYNC};
use super::pat;

/// The device-fixed IV.
pub const FIXED_IV: [u8; 16] = [
    0xEC, 0x8F, 0x4B, 0x6A, 0xD9, 0x2A, 0x36, 0x89, 0x2B, 0xDF, 0xB6, 0x18, 0xFC, 0x25, 0x5E, 0xFC,
];

/// transport_scrambling_control of a link-encrypted packet.
const TSC_LINK: u8 = 0x01;

/// AES-128-CBC decryption of whole blocks in place (`data.len()` a multiple of 16;
/// a partial last block is left alone).
fn cbc_decrypt(aes: &Aes128, iv: &[u8; 16], data: &mut [u8]) {
    let (blocks, _) = Block::<Aes128>::slice_as_chunks_mut(data);
    cbc::Decryptor::<&Aes128>::inner_iv_init(aes, iv.into()).decrypt_blocks(blocks);
}

/// Decrypt one packet's payload (`s` = payload offset) in place.
fn decrypt_payload(aes: &Aes128, payload: &mut [u8]) {
    let n = payload.len() / 16 * 16;
    let (body, tail) = payload.split_at_mut(n);
    // the last ciphertext block feeds the tail; keep it before CBC overwrites it
    let prev: [u8; 16] = body.last_chunk::<16>().copied().unwrap_or(FIXED_IV);
    cbc_decrypt(aes, &FIXED_IV, body);
    if !tail.is_empty() {
        let mut ks = Block::<Aes128>::from(prev);
        aes.encrypt_block(&mut ks);
        tail.iter_mut().zip(ks.iter()).for_each(|(b, k)| *b ^= k);
    }
}

/// Decrypt every link-encrypted packet of a 188-aligned buffer in place and clear
/// its transport_scrambling_control; other packets are left alone.
///
/// Stops at the first packet without a sync byte (lost sync: the rest is left as
/// is). A link-encrypted packet without payload only gets its TSC cleared.
pub fn decrypt(key: &[u8; 16], buf: &mut [u8]) {
    let aes = Aes128::new(key.into());
    for p in buf.as_chunks_mut::<PACKET>().0 {
        if p[0] != SYNC {
            break;
        }
        if packet::tsc(p) != TSC_LINK {
            continue;
        }
        if let Some(s) = packet::payload_offset(p) {
            decrypt_payload(&aes, &mut p[s..]);
        }
        p[3] &= 0x3F;
    }
}

/// Which key decrypts one raw (still link-encrypted) PID-0 packet to a CRC-valid PAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyVerdict {
    /// Not a link-encrypted section start: proves nothing.
    NotATest,
    /// The current key works.
    Current,
    /// Only this alternate key works (the device still uses an earlier session's key,
    /// one handed out to a process that ended before proving it, or switched to the
    /// key of its new session).
    Alternate([u8; 16]),
    /// Neither does.
    Neither,
}

/// Whether `key` turns the raw packet into a CRC-valid single-packet PAT.
fn decrypts_pat(raw: &[u8; PACKET], key: &[u8; 16]) -> bool {
    let mut t = *raw;
    decrypt(key, &mut t);
    pat::pat_section(&t).is_some()
}

/// The device keeps encrypting with the FIRST key it handed out after power-up; a
/// later key request returns a key it never applies. Test `raw` with `current`, then
/// `alternate`.
///
/// Hardware 2026-10-18: a warm re-open (new process, no re-plug) streamed 85 MB whose
/// PATs all failed the CRC under the fresh key, so hosts saw no PSI and no picture;
/// the key a previous session verified decrypted them. Run this on the raw PID-0
/// packets BEFORE their buffer is decrypted, so that buffer already gets the right
/// key. An `alternate` equal to `current` is not tried twice.
pub fn check_key<'a>(
    raw: &[u8],
    current: &[u8; 16],
    alternates: impl IntoIterator<Item = &'a [u8; 16]>,
) -> KeyVerdict {
    let Some(raw) = raw.first_chunk::<PACKET>() else {
        return KeyVerdict::NotATest;
    };
    if packet::tsc(raw) != TSC_LINK || !packet::pusi(raw) {
        return KeyVerdict::NotATest;
    }
    if decrypts_pat(raw, current) {
        return KeyVerdict::Current;
    }
    alternates
        .into_iter()
        .find(|alt| *alt != current && decrypts_pat(raw, alt))
        .map_or(KeyVerdict::Neither, |k| KeyVerdict::Alternate(*k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::testutil::{Rng, link_encrypt, make_pat_packet, region_encrypt};

    const KA: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    const KB: [u8; 16] = [
        0xA0, 0xB1, 0xC2, 0xD3, 0xE4, 0xF5, 0x06, 0x17, 0x28, 0x39, 0x4A, 0x5B, 0x6C, 0x7D, 0x8E,
        0x9F,
    ];
    const BS: [(u16, u16); 2] = [(0, 0x0010), (236, 0x0401)];

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // ---- AES-CBC self-test ----

    /// NIST SP800-38A F.2.2 CBC-AES128.Decrypt (block 1 alone, then all).
    #[test]
    fn aes128_cbc_decrypt_nist_f22() {
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let aes = Aes128::new(&key.into());
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let mut buf = hex(concat!(
            "7649abac8119b246cee98e9b12e9197d",
            "5086cb9b507219ee95db113a917678b2",
            "73bed6b8e3c1743b7116e69e22229516",
            "3ff1caa1681fac09120eca307586e1a7"
        ));
        let pt = hex(concat!(
            "6bc1bee22e409f96e93d7e117393172a",
            "ae2d8a571e03ac9c9eb76fac45af8e51",
            "30c81c46a35ce411e5fbc1191a0a52ef",
            "f69f2445df4f9b17ad2b417be66c3710"
        ));
        let mut one = buf[..16].to_vec();
        cbc_decrypt(&aes, &iv, &mut one);
        assert_eq!(one, pt[..16], "block 1");
        cbc_decrypt(&aes, &iv, &mut buf);
        assert_eq!(buf, pt, "all four blocks (chaining)");
    }

    /// TSC=01, afc=1 packet: encrypt payload [4..188], decrypt, compare.
    #[test]
    fn cbc_cfb_packet_round_trip_184_byte_payload() {
        let ltsc: [u8; 16] = std::array::from_fn(|i| (i * 3 + 5) as u8);
        let mut packet = [0u8; PACKET];
        packet[..4].copy_from_slice(&[0x47, 0x12, 0x34, 0x50]);
        for (i, b) in packet.iter_mut().enumerate().skip(4) {
            *b = (i * 7) as u8;
        }
        let mut orig = packet;
        region_encrypt(&ltsc, &mut packet[4..], &FIXED_IV);
        assert_ne!(packet, orig);
        decrypt(&ltsc, &mut packet);
        orig[3] &= 0x3F;
        assert_eq!(packet, orig);
    }

    // ---- beyond the selftest ----

    /// Known answers (key 00..0f; the
    /// "ciphertext" byte at packet offset i is i*7): whole blocks + an 8-byte tail,
    /// an adaptation field + a 12-byte tail, and a 13-byte tail-only payload (its
    /// keystream comes from the IV).
    #[test]
    fn known_answers_from_the_c_driver() {
        const WANT: [(Option<u8>, &str); 3] = [
            (
                None,
                concat!(
                    "4701001572f0e1325d472569e88c02ba3264d2d28e2b40a6c01d65b61f3938b1",
                    "1a8de3eb91599bde46b472993571a665ffc76dad6a3d362c8b04e825fb1d98c4",
                    "58fa86fe378c5e34d0f212edab01589c42f564d8081d5f217dd23184ad8d97e0",
                    "e6d2765335595d7b6412728330b44c525e541215b42d02bdb7d25357690c9a88",
                    "5803e0e3f91e8f828152c3d60d21ae2cf90b4310bfaffbe96fa10ecbf20b8194",
                    "882934625f29acc665999405c01702eb41a9066837c2bb2ca13c9a61",
                ),
            ),
            (
                Some(11),
                concat!(
                    "470100350bffffffffffffffffffffff87c7ca2a8216d5fbd8241322a8c20df7",
                    "374454f38b904b15aa2d5277a617535b79c192bfb080724754fe0e089af2271f",
                    "f871736caf63a1af91a781c4f468fd29beb8e4677992d24fcaf3138bc35f3e6a",
                    "3d8e2ee08e4e3a040b06698f58190f5bafc8ac496b28f6a80a70512b93421daf",
                    "ae8205e3327649c22e3084e2027a48a00d7ebc76604d2d6cddadd3422234da42",
                    "3ba597f7f050abf19d5795e1c7ca8b49b12edf4b2e3f700613b84ee5",
                ),
            ),
            (
                Some(170),
                concat!(
                    "47010035aaffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    "fffffffffffffffffffffffffffffff3d6d9f6a9fffbc560e77b2b00",
                ),
            ),
        ];
        let key: [u8; 16] = std::array::from_fn(|i| i as u8);
        for (af_len, want) in WANT {
            let mut p = [0u8; PACKET];
            let s = match af_len {
                None => {
                    p[..4].copy_from_slice(&[SYNC, 0x01, 0x00, 0x55]);
                    4
                }
                Some(n) => {
                    p[..5].copy_from_slice(&[SYNC, 0x01, 0x00, 0x75, n]);
                    let s = 5 + usize::from(n);
                    p[5..s].fill(0xFF);
                    s
                }
            };
            for (i, b) in p.iter_mut().enumerate().skip(s) {
                *b = (i * 7) as u8;
            }
            decrypt(&key, &mut p);
            assert_eq!(p[..], hex(want)[..], "adaptation field {af_len:?}");
        }
    }

    /// Every payload size: whole blocks only (rest 0), blocks + tail, tail only (the
    /// IV feeds the keystream), and adaptation fields of every length.
    #[test]
    fn round_trip_every_adaptation_field_length() {
        let mut rng = Rng(0x5EED);
        for af_len in 0..=183usize {
            let mut p = [0u8; PACKET];
            rng.fill(&mut p);
            p[0] = SYNC;
            p[3] = 0x70 | (af_len as u8 & 0x0F); // TSC 01, afc 3
            p[4] = af_len as u8;
            let mut plain = p;
            plain[3] &= 0x3F;
            let s = 5 + af_len;
            if s < PACKET {
                region_encrypt(&KA, &mut p[s..], &FIXED_IV);
            }
            decrypt(&KA, &mut p);
            assert_eq!(p, plain, "adaptation_field_length {af_len}");
        }
    }

    /// A 12-byte tail (payload 172 = 10 blocks + 12): its keystream is AES(last
    /// ciphertext block), not AES(last plaintext block).
    #[test]
    fn tail_uses_last_ciphertext_block() {
        let mut p = [0u8; PACKET];
        p[..5].copy_from_slice(&[SYNC, 0x01, 0x00, 0x70, 11]); // af 11 -> payload at 16
        let plain = p;
        let mut enc = p;
        region_encrypt(&KA, &mut enc[16..], &FIXED_IV);
        // the tail by hand: AES_K(C_last) XOR plaintext tail
        let mut ks = Block::<Aes128>::from(*enc[16..176].last_chunk::<16>().unwrap());
        Aes128::new(&KA.into()).encrypt_block(&mut ks);
        for i in 0..12 {
            assert_eq!(enc[176 + i], plain[176 + i] ^ ks[i]);
        }
        decrypt(&KA, &mut enc);
        let mut want = plain;
        want[3] &= 0x3F;
        assert_eq!(enc, want);
    }

    #[test]
    fn only_tsc_01_is_touched_and_sync_loss_stops() {
        let mut a = [0x11u8; PACKET];
        a[..4].copy_from_slice(&[SYNC, 0x01, 0x00, 0x10]); // TSC 00
        let mut b = [0x22u8; PACKET];
        b[..4].copy_from_slice(&[SYNC, 0x01, 0x00, 0x90]); // TSC 10 (broadcast scrambling)
        let mut c = [0x33u8; PACKET];
        c[..5].copy_from_slice(&[SYNC, 0x01, 0x00, 0x60, 183]); // TSC 01, AF only
        let mut d = [0x44u8; PACKET];
        d[..4].copy_from_slice(&[SYNC, 0x01, 0x00, 0x50]); // TSC 01 after a lost sync
        let mut buf = [a, b, c, [0u8; PACKET], d].concat();
        buf[3 * PACKET] = 0x00; // no sync byte
        let before = buf.clone();
        decrypt(&KA, &mut buf);
        assert_eq!(
            buf[..2 * PACKET],
            before[..2 * PACKET],
            "TSC 00 / 10 left alone"
        );
        assert_eq!(
            buf[2 * PACKET + 3],
            0x20,
            "AF-only link packet: TSC cleared"
        );
        assert_eq!(
            buf[2 * PACKET + 4..3 * PACKET],
            before[2 * PACKET + 4..3 * PACKET]
        );
        assert_eq!(
            buf[3 * PACKET..],
            before[3 * PACKET..],
            "nothing after the lost sync"
        );
        // a trailing partial packet is ignored
        let mut part = d[..100].to_vec();
        decrypt(&KA, &mut part);
        assert_eq!(part, d[..100]);
    }

    // ---- the link-key check ----

    fn encrypted_pat() -> [u8; PACKET] {
        let mut enc = make_pat_packet(&BS, 0, false);
        link_encrypt(&KA, &mut enc); // the device applies KA
        enc
    }

    #[test]
    fn encrypt_decrypt_round_trip_gives_a_valid_pat() {
        let enc = encrypted_pat();
        assert!(pat::pat_section(&enc).is_none());
        let mut t = enc;
        decrypt(&KA, &mut t);
        assert!(pat::pat_section(&t).is_some());
    }

    #[test]
    fn fresh_key_decrypts() {
        assert_eq!(check_key(&encrypted_pat(), &KA, &[]), KeyVerdict::Current);
        assert_eq!(check_key(&encrypted_pat(), &KA, &[KB]), KeyVerdict::Current);
    }

    #[test]
    fn warm_reopen_saved_key_decrypts() {
        assert_eq!(
            check_key(&encrypted_pat(), &KB, &[KA]),
            KeyVerdict::Alternate(KA)
        );
        // a key handed out to a process that ended before proving it
        let kc = [7u8; 16];
        assert_eq!(
            check_key(&encrypted_pat(), &KB, &[kc, KA]),
            KeyVerdict::Alternate(KA)
        );
    }

    #[test]
    fn no_working_key() {
        assert_eq!(check_key(&encrypted_pat(), &KB, &[]), KeyVerdict::Neither);
        assert_eq!(check_key(&encrypted_pat(), &KB, &[KB]), KeyVerdict::Neither);
    }

    #[test]
    fn unencrypted_pat_is_not_a_key_test() {
        let clear = make_pat_packet(&BS, 0, false);
        assert_eq!(check_key(&clear, &KB, &[KA]), KeyVerdict::NotATest);
        let mut no_start = encrypted_pat();
        no_start[1] &= !0x40;
        assert_eq!(check_key(&no_start, &KA, &[]), KeyVerdict::NotATest);
        assert_eq!(
            check_key(&encrypted_pat()[..187], &KA, &[]),
            KeyVerdict::NotATest
        );
    }
}
