//! The quantization ranges of ASTC's integer sequences and the fixed tables
//! derived from them: trit and quint packing, and the unquantization of a
//! stored weight to `0..=64` and of a stored color value to `0..=255`.

/// How the most significant part of every value in a range is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Base {
    /// No most significant part: the value is its low bits.
    Bits,
    /// A base-3 digit, five packed into 8 bits.
    Trit,
    /// A base-5 digit, three packed into 7 bits.
    Quint,
}

/// The 21 ranges an integer sequence is quantized to, fewest levels first:
/// the base of each value's high part and its count of low bits. Weights
/// take the first [`WEIGHT_RANGES`], color values those from
/// [`MIN_COLOR_RANGE`] on.
pub(super) const RANGES: [(Base, u32); 21] = [
    (Base::Bits, 1),
    (Base::Trit, 0),
    (Base::Bits, 2),
    (Base::Quint, 0),
    (Base::Trit, 1),
    (Base::Bits, 3),
    (Base::Quint, 1),
    (Base::Trit, 2),
    (Base::Bits, 4),
    (Base::Quint, 2),
    (Base::Trit, 3),
    (Base::Bits, 5),
    (Base::Quint, 3),
    (Base::Trit, 4),
    (Base::Bits, 6),
    (Base::Quint, 4),
    (Base::Trit, 5),
    (Base::Bits, 7),
    (Base::Quint, 5),
    (Base::Trit, 6),
    (Base::Bits, 8),
];

/// The weight ranges: `0..2` up to `0..32`.
pub(super) const WEIGHT_RANGES: usize = 12;

/// The smallest color range, `0..6`; a block whose color values would need
/// fewer levels is illegal.
pub(super) const MIN_COLOR_RANGE: usize = 4;

/// The number of levels range `range` holds.
pub(super) const fn levels(range: usize) -> u32 {
    let (base, bits) = RANGES[range];
    match base {
        Base::Bits => 1 << bits,
        Base::Trit => 3 << bits,
        Base::Quint => 5 << bits,
    }
}

/// The bits `count` values of range `range` take in an integer sequence.
pub(super) const fn sequence_bits(range: usize, count: u32) -> u32 {
    let (base, bits) = RANGES[range];
    count * bits
        + match base {
            Base::Bits => 0,
            Base::Trit => (8 * count).div_ceil(5),
            Base::Quint => (7 * count).div_ceil(3),
        }
}

/// Bit `i` of `v`.
const fn bit(v: u32, i: u32) -> u32 {
    (v >> i) & 1
}

/// The five trits an 8-bit packed block holds, as the spec unpacks them.
const fn unpack_trits(t: u32) -> [u8; 5] {
    let (c, t4, t3);
    if (t >> 2) & 7 == 7 {
        c = (((t >> 5) & 7) << 2) | (t & 3);
        t4 = 2;
        t3 = 2;
    } else {
        c = t & 0x1f;
        if (t >> 5) & 3 == 3 {
            t4 = 2;
            t3 = (t >> 7) & 1;
        } else {
            t4 = (t >> 7) & 1;
            t3 = (t >> 5) & 3;
        }
    }
    let (t2, t1, t0);
    if c & 3 == 3 {
        t2 = 2;
        t1 = bit(c, 4);
        t0 = (bit(c, 3) << 1) | (bit(c, 2) & !bit(c, 3) & 1);
    } else if (c >> 2) & 3 == 3 {
        t2 = 2;
        t1 = 2;
        t0 = c & 3;
    } else {
        t2 = bit(c, 4);
        t1 = (c >> 2) & 3;
        t0 = (bit(c, 1) << 1) | (bit(c, 0) & !bit(c, 1) & 1);
    }
    [t0 as u8, t1 as u8, t2 as u8, t3 as u8, t4 as u8]
}

/// The three quints a 7-bit packed block holds, as the spec unpacks them.
const fn unpack_quints(q: u32) -> [u8; 3] {
    if (q >> 1) & 3 == 3 && (q >> 5) & 3 == 0 {
        let q2 =
            (bit(q, 0) << 2) | ((bit(q, 4) & !bit(q, 0) & 1) << 1) | (bit(q, 3) & !bit(q, 0) & 1);
        return [4, 4, q2 as u8];
    }
    let (q2, c);
    if (q >> 1) & 3 == 3 {
        q2 = 4;
        c = (((q >> 3) & 3) << 3) | ((!(q >> 5) & 3) << 1) | bit(q, 0);
    } else {
        q2 = (q >> 5) & 3;
        c = q & 0x1f;
    }
    let (q1, q0) = if c & 7 == 5 {
        (4, (c >> 3) & 3)
    } else {
        ((c >> 3) & 3, c & 7)
    };
    [q0 as u8, q1 as u8, q2 as u8]
}

/// Every packed trit block unpacked.
pub(super) const TRITS: [[u8; 5]; 256] = {
    let mut out = [[0; 5]; 256];
    let mut t = 0;
    while t < 256 {
        out[t] = unpack_trits(t as u32);
        t += 1;
    }
    out
};

/// Every packed quint block unpacked.
pub(super) const QUINTS: [[u8; 3]; 128] = {
    let mut out = [[0; 3]; 128];
    let mut q = 0;
    while q < 128 {
        out[q] = unpack_quints(q as u32);
        q += 1;
    }
    out
};

/// The smallest packed block for every five trits, indexed by
/// `t0 + 3·t1 + 9·t2 + 27·t3 + 81·t4`. Smallest, so trailing zero trits
/// leave the high bits zero and a truncated final block reads back exactly.
pub(super) const TRIT_PACK: [u8; 243] = {
    let mut out = [0u8; 243];
    let mut seen = [false; 243];
    let mut t = 0;
    while t < 256 {
        let d = TRITS[t];
        let i = d[0] as usize
            + 3 * d[1] as usize
            + 9 * d[2] as usize
            + 27 * d[3] as usize
            + 81 * d[4] as usize;
        if !seen[i] {
            seen[i] = true;
            out[i] = t as u8;
        }
        t += 1;
    }
    out
};

/// The smallest packed block for every three quints, indexed by
/// `q0 + 5·q1 + 25·q2`.
pub(super) const QUINT_PACK: [u8; 125] = {
    let mut out = [0u8; 125];
    let mut seen = [false; 125];
    let mut q = 0;
    while q < 128 {
        let d = QUINTS[q];
        let i = d[0] as usize + 5 * d[1] as usize + 25 * d[2] as usize;
        if !seen[i] {
            seen[i] = true;
            out[i] = q as u8;
        }
        q += 1;
    }
    out
};

/// `v`, `n` bits wide, widened to `to` bits by repeating it from its most
/// significant bit.
const fn replicate(v: u32, n: u32, to: u32) -> u32 {
    let mut out = 0;
    let mut filled = 0;
    while filled < to {
        out = (out << n) | v;
        filled += n;
    }
    out >> (filled - to)
}

/// Stored weight `v` of range `range` unquantized to `0..=64`.
const fn unquantize_weight(range: usize, v: u32) -> u8 {
    let (base, bits) = RANGES[range];
    let d = v >> bits;
    let m = v & ((1 << bits) - 1);
    let unq = match (base, bits) {
        (Base::Bits, _) => replicate(v, bits, 6),
        (Base::Trit, 0) => [0, 32, 63][v as usize],
        (Base::Quint, 0) => [0, 16, 32, 47, 63][v as usize],
        _ => {
            let (c, b) = match (base, bits) {
                (Base::Trit, 1) => (50, 0),
                (Base::Quint, 1) => (28, 0),
                (Base::Trit, 2) => (23, bit(m, 1) * 0x45),
                (Base::Quint, 2) => (13, bit(m, 1) * 0x42),
                _ => (11, bit(m, 2) * 0x42 + bit(m, 1) * 0x21),
            };
            let a = if bit(m, 0) == 1 { 0x7f } else { 0 };
            let u = (d * c + b) ^ a;
            (a & 0x20) | (u >> 2)
        }
    };
    (if unq > 32 { unq + 1 } else { unq }) as u8
}

/// Stored color value `v` of range `range` unquantized to `0..=255`; zero
/// for the ranges below [`MIN_COLOR_RANGE`], which no legal block holds.
const fn unquantize_color(range: usize, v: u32) -> u8 {
    if range < MIN_COLOR_RANGE {
        return 0;
    }
    let (base, bits) = RANGES[range];
    if let Base::Bits = base {
        return replicate(v, bits, 8) as u8;
    }
    let d = v >> bits;
    let m = v & ((1 << bits) - 1);
    let (c, b) = match (base, bits) {
        (Base::Trit, 1) => (204, 0),
        (Base::Quint, 1) => (113, 0),
        (Base::Trit, 2) => (93, bit(m, 1) * 0x116),
        (Base::Quint, 2) => (54, bit(m, 1) * 0x10c),
        (Base::Trit, 3) => (44, bit(m, 2) * 0x10a + bit(m, 1) * 0x085),
        (Base::Quint, 3) => (26, bit(m, 2) * 0x105 + bit(m, 1) * 0x082),
        (Base::Trit, 4) => (
            22,
            bit(m, 3) * 0x104 + bit(m, 2) * 0x082 + bit(m, 1) * 0x041,
        ),
        (Base::Quint, 4) => (
            13,
            bit(m, 3) * 0x102 + bit(m, 2) * 0x081 + bit(m, 1) * 0x040,
        ),
        (Base::Trit, 5) => {
            let b = bit(m, 4) * 0x102 + bit(m, 3) * 0x081 + bit(m, 2) * 0x040 + bit(m, 1) * 0x020;
            (11, b)
        }
        (Base::Quint, 5) => {
            let b = bit(m, 4) * 0x101 + bit(m, 3) * 0x080 + bit(m, 2) * 0x040 + bit(m, 1) * 0x020;
            (6, b)
        }
        _ => {
            let b = bit(m, 5) * 0x101
                + bit(m, 4) * 0x080
                + bit(m, 3) * 0x040
                + bit(m, 2) * 0x020
                + bit(m, 1) * 0x010;
            (5, b)
        }
    };
    let a = if bit(m, 0) == 1 { 0x1ff } else { 0 };
    let u = (d * c + b) ^ a;
    ((a & 0x80) | (u >> 2)) as u8
}

/// Every stored weight of every weight range unquantized to `0..=64`.
pub(super) const WEIGHTS: [[u8; 32]; WEIGHT_RANGES] = {
    let mut out = [[0; 32]; WEIGHT_RANGES];
    let mut r = 0;
    while r < WEIGHT_RANGES {
        let mut v = 0;
        while v < levels(r) {
            out[r][v as usize] = unquantize_weight(r, v);
            v += 1;
        }
        r += 1;
    }
    out
};

/// Every stored color value of every range unquantized to `0..=255`.
pub(super) const COLORS: [[u8; 256]; 21] = {
    let mut out = [[0; 256]; 21];
    let mut r = MIN_COLOR_RANGE;
    while r < 21 {
        let mut v = 0;
        while v < levels(r) {
            out[r][v as usize] = unquantize_color(r, v);
            v += 1;
        }
        r += 1;
    }
    out
};
