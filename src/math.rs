//! `Math`'s transcendental functions, as fdlibm computes them (Sun's
//! freely distributable libm, 5.3), which is what V8 ships
//! (`base/ieee754.cc`). The platform's `libm` gives results that differ in
//! the last bit between systems (Apple's `tan(1)`, `asin(0.5)` and
//! `acos(0.5)` against glibc's), so a script's numbers would depend on the
//! host. These are bit-for-bit Node's on every platform.
//!
//! Each function is a direct port of its fdlibm source (`k_sin.c`,
//! `k_cos.c`, `k_tan.c`, `e_rem_pio2.c`, `k_rem_pio2.c`, `s_sin.c`,
//! `s_cos.c`, `s_tan.c`, `e_asin.c`, `e_acos.c`, `s_atan.c`, `e_atan2.c`,
//! `e_exp.c`, `e_log.c`, `e_pow.c`, `s_scalbn.c`), with the same operation
//! order, since that is what fixes the rounding. `sqrt` is exact everywhere
//! and stays the hardware's.
//!
//! So fdlibm's idioms stay as written, where Clippy would change them:
//! `x - x` and `x / x` make NaN or raise flags on purpose (`eq_op`), its
//! constants keep all their digits (`excessive_precision`), and some are
//! near `π`'s or `ln 2`'s without being them (`approx_constant`).
#![allow(clippy::eq_op, clippy::excessive_precision, clippy::approx_constant)]

/// The high 32 bits, as fdlibm's `__HI`.
#[inline(always)]
fn hi(x: f64) -> i32 {
    (x.to_bits() >> 32) as i32
}

/// The low 32 bits, as fdlibm's `__LO`.
#[inline(always)]
fn lo(x: f64) -> u32 {
    x.to_bits() as u32
}

#[inline(always)]
fn with_hi(x: f64, h: i32) -> f64 {
    f64::from_bits(((h as u32 as u64) << 32) | (x.to_bits() & 0xFFFF_FFFF))
}

#[inline(always)]
fn with_lo(x: f64, l: u32) -> f64 {
    f64::from_bits((x.to_bits() & 0xFFFF_FFFF_0000_0000) | l as u64)
}

#[inline(always)]
fn from_words(h: i32, l: u32) -> f64 {
    f64::from_bits(((h as u32 as u64) << 32) | l as u64)
}

const HUGE: f64 = 1.0e300;
const TINY: f64 = 1.0e-300;
const TWO54: f64 = 1.80143985094819840000e+16;
const TWOM54: f64 = 5.55111512312578270212e-17;
const TWO24: f64 = 1.67772160000000000000e+07;
const TWON24: f64 = 5.96046447753906250000e-08;

/// `x * 2^n`, exactly as fdlibm's `scalbn` rounds it (subnormal results
/// included).
fn scalbn(x: f64, n: i32) -> f64 {
    let mut x = x;
    let mut hx = hi(x);
    let lx = lo(x);
    let mut k = (hx & 0x7ff00000) >> 20;
    if k == 0 {
        if (lx | (hx & 0x7fffffff) as u32) == 0 {
            return x;
        }
        x *= TWO54;
        hx = hi(x);
        k = ((hx & 0x7ff00000) >> 20) - 54;
        if n < -50000 {
            return TINY * x;
        }
    }
    if k == 0x7ff {
        return x + x;
    }
    k += n;
    if k > 0x7fe {
        return HUGE * x.signum() * HUGE;
    }
    if k > 0 {
        return with_hi(x, (hx & 0x800fffff_u32 as i32) | (k << 20));
    }
    if k <= -54 {
        return if n > 50000 { HUGE * x.signum() * HUGE } else { TINY * x.signum() * TINY };
    }
    k += 54;
    with_hi(x, (hx & 0x800fffff_u32 as i32) | (k << 20)) * TWOM54
}

// ---- Trigonometry ----

/// sin on [-pi/4, pi/4], `x + y` the argument, `y` the tail of a reduction
/// (`iy` false: `y` is 0).
fn k_sin(x: f64, y: f64, iy: bool) -> f64 {
    const S1: f64 = -1.66666666666666324348e-01;
    const S2: f64 = 8.33333333332248946124e-03;
    const S3: f64 = -1.98412698298579493134e-04;
    const S4: f64 = 2.75573137070700676789e-06;
    const S5: f64 = -2.50507602534068634195e-08;
    const S6: f64 = 1.58969099521155010221e-10;
    let ix = hi(x) & 0x7fffffff;
    if ix < 0x3e400000 {
        // |x| < 2^-27
        return x;
    }
    let z = x * x;
    let v = z * x;
    let r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
    if !iy {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (0.5 * y - v * r) - y) - v * S1)
    }
}

/// cos on [-pi/4, pi/4], `x + y` the argument.
fn k_cos(x: f64, y: f64) -> f64 {
    const C1: f64 = 4.16666666666666019037e-02;
    const C2: f64 = -1.38888888888741095749e-03;
    const C3: f64 = 2.48015872894767294178e-05;
    const C4: f64 = -2.75573143513906633035e-07;
    const C5: f64 = 2.08757232129817482790e-09;
    const C6: f64 = -1.13596475577881948265e-11;
    let ix = hi(x) & 0x7fffffff;
    if ix < 0x3e400000 {
        return 1.0;
    }
    let z = x * x;
    let r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
    if ix < 0x3FD33333 {
        // |x| < 0.3
        1.0 - (0.5 * z - (z * r - x * y))
    } else {
        let qx = if ix > 0x3fe90000 {
            // x > 0.78125
            0.28125
        } else {
            from_words(ix - 0x00200000, 0)
        };
        let hz = 0.5 * z - qx;
        let a = 1.0 - qx;
        a - (hz - (z * r - x * y))
    }
}

/// tan on [-pi/4, pi/4], `x + y` the argument; `iy` 1 for tan, -1 for
/// -1/tan.
fn k_tan(x: f64, y: f64, iy: i32) -> f64 {
    const T: [f64; 13] = [
        3.33333333333334091986e-01,
        1.33333333333201242699e-01,
        5.39682539762260521377e-02,
        2.18694882948595424599e-02,
        8.86323982359930005737e-03,
        3.59207910759131235356e-03,
        1.45620945432529025516e-03,
        5.88041240820264096874e-04,
        2.46463134818469906812e-04,
        7.81794442939557092300e-05,
        7.14072491382608190305e-05,
        -1.85586374855275456654e-05,
        2.59073051863633712884e-05,
    ];
    const PIO4: f64 = 7.85398163397448278999e-01;
    const PIO4LO: f64 = 3.06161699786838301793e-17;
    let (mut x, mut y) = (x, y);
    let hx = hi(x);
    let ix = hx & 0x7fffffff;
    if ix < 0x3e300000 {
        // |x| < 2^-28
        if ((ix as u32 | lo(x)) | (iy + 1) as u32) == 0 {
            return 1.0 / x.abs();
        }
        if iy == 1 {
            return x;
        }
        // -1/(x+y), carefully.
        let w = x + y;
        let z = with_lo(w, 0);
        let v = y - (z - x);
        let a = -1.0 / w;
        let t = with_lo(a, 0);
        let s = 1.0 + t * z;
        return t + a * (s + t * v);
    }
    if ix >= 0x3FE59428 {
        // |x| >= 0.6744
        if hx < 0 {
            x = -x;
            y = -y;
        }
        let z = PIO4 - x;
        let w = PIO4LO - y;
        x = z + w;
        y = 0.0;
    }
    let z = x * x;
    let w = z * z;
    let r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    let v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    let s = z * x;
    let mut r = y + z * (s * (r + v) + y);
    r += T[0] * s;
    let w = x + r;
    if ix >= 0x3FE59428 {
        let v = iy as f64;
        return (1 - ((hx >> 30) & 2)) as f64 * (v - 2.0 * (x - (w * w / (w + v) - r)));
    }
    if iy == 1 {
        return w;
    }
    // -1.0/(x+r), accurately.
    let z = with_lo(w, 0);
    let v = r - (z - x);
    let a = -1.0 / w;
    let t = with_lo(a, 0);
    let s = 1.0 + t * z;
    t + a * (s + t * v)
}

/// 2/pi, 24 bits per entry: `k_rem_pio2`'s table.
const IPIO2: [i32; 66] = [
    0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
    0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
    0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
    0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
    0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
    0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
    0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
    0x73A8C9, 0x60E27B, 0xC08C6B,
];

/// pi/2 in 24-bit pieces.
const PIO2: [f64; 8] = [
    1.57079625129699707031e+00,
    7.54978941586159635335e-08,
    5.39030252995776476554e-15,
    3.28200341580791294123e-22,
    1.27065575308067607349e-29,
    1.22933308981111328932e-36,
    2.73370053816464559624e-44,
    2.16741683877804979684e-51,
];

/// Reduce a large argument: `x` is `nx` doubles of 24 bits each (the
/// mantissa of |x| in pieces), scaled by `2^e0`. Returns `n mod 8` and
/// writes the remainder `(|x| mod pi/2)` as `y[0] + y[1]`, with `prec` 2
/// (two doubles).
fn k_rem_pio2(x: &[f64], y: &mut [f64; 2], e0: i32, nx: usize) -> i32 {
    const JK: usize = 4; // init_jk[prec = 2]
    const JP: usize = JK;
    let mut iq = [0i32; 20];
    let mut f = [0.0f64; 20];
    let mut fq = [0.0f64; 20];
    let mut q = [0.0f64; 20];

    let jx = nx - 1;
    let jv = ((e0 - 3) / 24).max(0);
    let mut q0 = e0 - 24 * (jv + 1);

    // f[0..=jx+jk] = ipio2[jv-jx..]
    let mut j = jv - jx as i32;
    let m = jx + JK;
    for fi in f.iter_mut().take(m + 1) {
        *fi = if j < 0 { 0.0 } else { IPIO2[j as usize] as f64 };
        j += 1;
    }
    // q[0..=jk]
    for i in 0..=JK {
        let mut fw = 0.0;
        for j in 0..=jx {
            fw += x[j] * f[jx + i - j];
        }
        q[i] = fw;
    }

    let mut jz = JK;
    let mut n;
    let mut ih;
    let mut z;
    loop {
        // Distill q[] into iq[] reversingly.
        z = q[jz];
        let mut i = 0;
        let mut j = jz;
        while j > 0 {
            let fw = ((TWON24 * z) as i32) as f64;
            iq[i] = (z - TWO24 * fw) as i32;
            z = q[j - 1] + fw;
            i += 1;
            j -= 1;
        }

        // n
        z = scalbn(z, q0);
        z -= 8.0 * (z * 0.125).floor();
        n = z as i32;
        z -= n as f64;
        ih = 0;
        if q0 > 0 {
            let i = iq[jz - 1] >> (24 - q0);
            n += i;
            iq[jz - 1] -= i << (24 - q0);
            ih = iq[jz - 1] >> (23 - q0);
        } else if q0 == 0 {
            ih = iq[jz - 1] >> 23;
        } else if z >= 0.5 {
            ih = 2;
        }
        if ih > 0 {
            n += 1;
            let mut carry = 0;
            for item in iq.iter_mut().take(jz) {
                let j = *item;
                if carry == 0 {
                    if j != 0 {
                        carry = 1;
                        *item = 0x1000000 - j;
                    }
                } else {
                    *item = 0xffffff - j;
                }
            }
            if q0 > 0 {
                match q0 {
                    1 => iq[jz - 1] &= 0x7fffff,
                    2 => iq[jz - 1] &= 0x3fffff,
                    _ => {}
                }
            }
            if ih == 2 {
                z = 1.0 - z;
                if carry != 0 {
                    z -= scalbn(1.0, q0);
                }
            }
        }

        // Recompute if z == 0 and the remaining terms are all zero.
        if z == 0.0 {
            let mut j = 0;
            let mut i = jz as i32 - 1;
            while i >= JK as i32 {
                j |= iq[i as usize];
                i -= 1;
            }
            if j == 0 {
                let mut k = 1;
                while iq[JK - k] == 0 {
                    k += 1;
                }
                for i in jz + 1..=jz + k {
                    f[jx + i] = IPIO2[jv as usize + i] as f64;
                    let mut fw = 0.0;
                    for j in 0..=jx {
                        fw += x[j] * f[jx + i - j];
                    }
                    q[i] = fw;
                }
                jz += k;
                continue;
            }
        }
        break;
    }

    // Chop off zero terms.
    if z == 0.0 {
        jz -= 1;
        q0 -= 24;
        while iq[jz] == 0 {
            jz -= 1;
            q0 -= 24;
        }
    } else {
        z = scalbn(z, -q0);
        if z >= TWO24 {
            let fw = ((TWON24 * z) as i32) as f64;
            iq[jz] = (z - TWO24 * fw) as i32;
            jz += 1;
            q0 += 24;
            iq[jz] = fw as i32;
        } else {
            iq[jz] = z as i32;
        }
    }

    // The chunks as doubles.
    let mut fw = scalbn(1.0, q0);
    let mut i = jz as i32;
    while i >= 0 {
        q[i as usize] = fw * iq[i as usize] as f64;
        fw *= TWON24;
        i -= 1;
    }

    // PIO2[0..=jp] * q[jz..=0]
    let mut i = jz as i32;
    while i >= 0 {
        let iu = i as usize;
        let mut fw = 0.0;
        let mut k = 0;
        while k <= JP && k <= jz - iu {
            fw += PIO2[k] * q[iu + k];
            k += 1;
        }
        fq[jz - iu] = fw;
        i -= 1;
    }

    // Compress fq[] into y[] (prec 2).
    let mut fw = 0.0;
    let mut i = jz as i32;
    while i >= 0 {
        fw += fq[i as usize];
        i -= 1;
    }
    y[0] = if ih == 0 { fw } else { -fw };
    let mut fw = fq[0] - fw;
    for item in fq.iter().take(jz + 1).skip(1) {
        fw += item;
    }
    y[1] = if ih == 0 { fw } else { -fw };
    n & 7
}

/// The high words of `n * pi/2`, n = 1..=32: where the medium-size
/// reduction must check for cancellation.
const NPIO2_HW: [i32; 32] = [
    0x3FF921FB, 0x400921FB, 0x4012D97C, 0x401921FB, 0x401F6A7A, 0x4022D97C, 0x4025FDBB, 0x402921FB,
    0x402C463A, 0x402F6A7A, 0x4031475C, 0x4032D97C, 0x40346B9C, 0x4035FDBB, 0x40378FDB, 0x403921FB,
    0x403AB41B, 0x403C463A, 0x403DD85A, 0x403F6A7A, 0x40407E4C, 0x4041475C, 0x4042106C, 0x4042D97C,
    0x4043A28C, 0x40446B9C, 0x404534AC, 0x4045FDBB, 0x4046C6CB, 0x40478FDB, 0x404858EB, 0x404921FB,
];

/// `x mod pi/2`: the quadrant `n` (mod 4 matters) and the remainder as
/// `y[0] + y[1]`.
fn rem_pio2(x: f64, y: &mut [f64; 2]) -> i32 {
    const INVPIO2: f64 = 6.36619772367581382433e-01;
    const PIO2_1: f64 = 1.57079632673412561417e+00;
    const PIO2_1T: f64 = 6.07710050650619224932e-11;
    const PIO2_2: f64 = 6.07710050630396597660e-11;
    const PIO2_2T: f64 = 2.02226624879595063154e-21;
    const PIO2_3: f64 = 2.02226624871116645580e-21;
    const PIO2_3T: f64 = 8.47842766036889956997e-32;

    let hx = hi(x);
    let ix = hx & 0x7fffffff;
    if ix <= 0x3fe921fb {
        // |x| <= pi/4, no reduction
        y[0] = x;
        y[1] = 0.0;
        return 0;
    }
    if ix < 0x4002d97c {
        // |x| < 3pi/4: n = +-1
        if hx > 0 {
            let mut z = x - PIO2_1;
            if ix != 0x3ff921fb {
                y[0] = z - PIO2_1T;
                y[1] = (z - y[0]) - PIO2_1T;
            } else {
                z -= PIO2_2;
                y[0] = z - PIO2_2T;
                y[1] = (z - y[0]) - PIO2_2T;
            }
            return 1;
        } else {
            let mut z = x + PIO2_1;
            if ix != 0x3ff921fb {
                y[0] = z + PIO2_1T;
                y[1] = (z - y[0]) + PIO2_1T;
            } else {
                z += PIO2_2;
                y[0] = z + PIO2_2T;
                y[1] = (z - y[0]) + PIO2_2T;
            }
            return -1;
        }
    }
    if ix <= 0x413921fb {
        // |x| <= 2^19 * pi/2, medium size
        let t = x.abs();
        let n = (t * INVPIO2 + 0.5) as i32;
        let fnn = n as f64;
        let mut r = t - fnn * PIO2_1;
        let mut w = fnn * PIO2_1T;
        if n < 32 && ix != NPIO2_HW[n as usize - 1] {
            y[0] = r - w;
        } else {
            let j = ix >> 20;
            y[0] = r - w;
            let i = j - ((hi(y[0]) >> 20) & 0x7ff);
            if i > 16 {
                // 2nd iteration, good to 118 bits
                let t = r;
                w = fnn * PIO2_2;
                r = t - w;
                w = fnn * PIO2_2T - ((t - r) - w);
                y[0] = r - w;
                let i = j - ((hi(y[0]) >> 20) & 0x7ff);
                if i > 49 {
                    // 3rd iteration, 151 bits
                    let t = r;
                    w = fnn * PIO2_3;
                    r = t - w;
                    w = fnn * PIO2_3T - ((t - r) - w);
                    y[0] = r - w;
                }
            }
        }
        y[1] = (r - y[0]) - w;
        if hx < 0 {
            y[0] = -y[0];
            y[1] = -y[1];
            return -n;
        }
        return n;
    }
    if ix >= 0x7ff00000 {
        // Infinity or NaN
        y[0] = x - x;
        y[1] = y[0];
        return 0;
    }
    // Large: z = |x| * 2^(23 - ilogb(x)), in 24-bit pieces.
    let e0 = (ix >> 20) - 1046;
    let mut z = from_words(ix - (e0 << 20), lo(x));
    let mut tx = [0.0f64; 3];
    for t in tx.iter_mut().take(2) {
        *t = (z as i32) as f64;
        z = (z - *t) * TWO24;
    }
    tx[2] = z;
    let mut nx = 3;
    while tx[nx - 1] == 0.0 {
        nx -= 1;
    }
    let n = k_rem_pio2(&tx, y, e0, nx);
    if hx < 0 {
        y[0] = -y[0];
        y[1] = -y[1];
        return -n;
    }
    n
}

pub fn sin(x: f64) -> f64 {
    let ix = hi(x) & 0x7fffffff;
    if ix <= 0x3fe921fb {
        return k_sin(x, 0.0, false);
    }
    if ix >= 0x7ff00000 {
        return x - x;
    }
    let mut y = [0.0; 2];
    match rem_pio2(x, &mut y) & 3 {
        0 => k_sin(y[0], y[1], true),
        1 => k_cos(y[0], y[1]),
        2 => -k_sin(y[0], y[1], true),
        _ => -k_cos(y[0], y[1]),
    }
}

pub fn cos(x: f64) -> f64 {
    let ix = hi(x) & 0x7fffffff;
    if ix <= 0x3fe921fb {
        return k_cos(x, 0.0);
    }
    if ix >= 0x7ff00000 {
        return x - x;
    }
    let mut y = [0.0; 2];
    match rem_pio2(x, &mut y) & 3 {
        0 => k_cos(y[0], y[1]),
        1 => -k_sin(y[0], y[1], true),
        2 => -k_cos(y[0], y[1]),
        _ => k_sin(y[0], y[1], true),
    }
}

pub fn tan(x: f64) -> f64 {
    let ix = hi(x) & 0x7fffffff;
    if ix <= 0x3fe921fb {
        return k_tan(x, 0.0, 1);
    }
    if ix >= 0x7ff00000 {
        return x - x;
    }
    let mut y = [0.0; 2];
    let n = rem_pio2(x, &mut y);
    k_tan(y[0], y[1], 1 - ((n & 1) << 1))
}

const PIO2_HI: f64 = 1.57079632679489655800e+00;
const PIO2_LO: f64 = 6.12323399573676603587e-17;
const PIO4_HI: f64 = 7.85398163397448278999e-01;
const PS0: f64 = 1.66666666666666657415e-01;
const PS1: f64 = -3.25565818622400915405e-01;
const PS2: f64 = 2.01212532134862925881e-01;
const PS3: f64 = -4.00555345006794114027e-02;
const PS4: f64 = 7.91534994289814532176e-04;
const PS5: f64 = 3.47933107596021167570e-05;
const QS1: f64 = -2.40339491173441421878e+00;
const QS2: f64 = 2.02094576023350569471e+00;
const QS3: f64 = -6.88283971605453293030e-01;
const QS4: f64 = 7.70381505559019352791e-02;

#[inline(always)]
fn asin_pq(t: f64) -> (f64, f64) {
    let p = t * (PS0 + t * (PS1 + t * (PS2 + t * (PS3 + t * (PS4 + t * PS5)))));
    let q = 1.0 + t * (QS1 + t * (QS2 + t * (QS3 + t * QS4)));
    (p, q)
}

pub fn asin(x: f64) -> f64 {
    let hx = hi(x);
    let ix = hx & 0x7fffffff;
    if ix >= 0x3ff00000 {
        // |x| >= 1
        if ((ix - 0x3ff00000) as u32 | lo(x)) == 0 {
            // asin(1) = +-pi/2 with inexact
            return x * PIO2_HI + x * PIO2_LO;
        }
        return (x - x) / (x - x);
    }
    if ix < 0x3fe00000 {
        // |x| < 0.5
        if ix < 0x3e400000 {
            // |x| < 2^-27
            return x;
        }
        let (p, q) = asin_pq(x * x);
        let w = p / q;
        return x + x * w;
    }
    // 1 > |x| >= 0.5
    let w = 1.0 - x.abs();
    let t = w * 0.5;
    let (p, q) = asin_pq(t);
    let s = t.sqrt();
    let t = if ix >= 0x3FEF3333 {
        // |x| > 0.975
        let w = p / q;
        PIO2_HI - (2.0 * (s + s * w) - PIO2_LO)
    } else {
        let w = with_lo(s, 0);
        let c = (t - w * w) / (s + w);
        let r = p / q;
        let p = 2.0 * s * r - (PIO2_LO - 2.0 * c);
        let q = PIO4_HI - 2.0 * w;
        PIO4_HI - (p - q)
    };
    if hx > 0 { t } else { -t }
}

pub fn acos(x: f64) -> f64 {
    const PI: f64 = 3.14159265358979311600e+00;
    let hx = hi(x);
    let ix = hx & 0x7fffffff;
    if ix >= 0x3ff00000 {
        // |x| >= 1
        if ((ix - 0x3ff00000) as u32 | lo(x)) == 0 {
            return if hx > 0 { 0.0 } else { PI + 2.0 * PIO2_LO };
        }
        return (x - x) / (x - x);
    }
    if ix < 0x3fe00000 {
        // |x| < 0.5
        if ix <= 0x3c600000 {
            // |x| < 2^-57
            return PIO2_HI + PIO2_LO;
        }
        let (p, q) = asin_pq(x * x);
        let r = p / q;
        PIO2_HI - (x - (PIO2_LO - x * r))
    } else if hx < 0 {
        // x < -0.5
        let z = (1.0 + x) * 0.5;
        let (p, q) = asin_pq(z);
        let s = z.sqrt();
        let r = p / q;
        let w = r * s - PIO2_LO;
        PI - 2.0 * (s + w)
    } else {
        // x > 0.5
        let z = (1.0 - x) * 0.5;
        let s = z.sqrt();
        let df = with_lo(s, 0);
        let c = (z - df * df) / (s + df);
        let (p, q) = asin_pq(z);
        let r = p / q;
        let w = r * s + c;
        2.0 * (df + w)
    }
}

const ATANHI: [f64; 4] = [
    4.63647609000806093515e-01,
    7.85398163397448278999e-01,
    9.82793723247329054082e-01,
    1.57079632679489655800e+00,
];
const ATANLO: [f64; 4] = [
    2.26987774529616870924e-17,
    3.06161699786838301793e-17,
    1.39033110312309984516e-17,
    6.12323399573676603587e-17,
];

pub fn atan(x: f64) -> f64 {
    const AT: [f64; 11] = [
        3.33333333333329318027e-01,
        -1.99999999998764832476e-01,
        1.42857142725034663711e-01,
        -1.11111104054623557880e-01,
        9.09088713343650656196e-02,
        -7.69187620504482999495e-02,
        6.66107313738753120669e-02,
        -5.83357013379057348645e-02,
        4.97687799461593236017e-02,
        -3.65315727442169155270e-02,
        1.62858201153657823623e-02,
    ];
    let mut x = x;
    let hx = hi(x);
    let ix = hx & 0x7fffffff;
    if ix >= 0x44100000 {
        // |x| >= 2^66
        if ix > 0x7ff00000 || (ix == 0x7ff00000 && lo(x) != 0) {
            return x + x;
        }
        return if hx > 0 { ATANHI[3] + ATANLO[3] } else { -ATANHI[3] - ATANLO[3] };
    }
    let id: i32;
    if ix < 0x3fdc0000 {
        // |x| < 0.4375
        if ix < 0x3e200000 {
            // |x| < 2^-29
            return x;
        }
        id = -1;
    } else {
        x = x.abs();
        if ix < 0x3ff30000 {
            // |x| < 1.1875
            if ix < 0x3fe60000 {
                // 7/16 <= |x| < 11/16
                id = 0;
                x = (2.0 * x - 1.0) / (2.0 + x);
            } else {
                // 11/16 <= |x| < 19/16
                id = 1;
                x = (x - 1.0) / (x + 1.0);
            }
        } else if ix < 0x40038000 {
            // |x| < 2.4375
            id = 2;
            x = (x - 1.5) / (1.0 + 1.5 * x);
        } else {
            // 2.4375 <= |x| < 2^66
            id = 3;
            x = -1.0 / x;
        }
    }
    let z = x * x;
    let w = z * z;
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    if id < 0 {
        return x - x * (s1 + s2);
    }
    let z = ATANHI[id as usize] - ((x * (s1 + s2) - ATANLO[id as usize]) - x);
    if hx < 0 { -z } else { z }
}

pub fn atan2(y: f64, x: f64) -> f64 {
    const PI_O_4: f64 = 7.8539816339744827900E-01;
    const PI_O_2: f64 = 1.5707963267948965580E+00;
    const PI: f64 = 3.1415926535897931160E+00;
    const PI_LO: f64 = 1.2246467991473531772E-16;
    let hx = hi(x);
    let ix = hx & 0x7fffffff;
    let lx = lo(x);
    let hy = hi(y);
    let iy = hy & 0x7fffffff;
    let ly = lo(y);
    if (ix as u32 | ((lx | lx.wrapping_neg()) >> 31)) > 0x7ff00000
        || (iy as u32 | ((ly | ly.wrapping_neg()) >> 31)) > 0x7ff00000
    {
        // x or y is NaN
        return x + y;
    }
    if ((hx - 0x3ff00000) as u32 | lx) == 0 {
        // x = 1
        return atan(y);
    }
    // 2 * sign(x) + sign(y)
    let m = ((hy >> 31) & 1) | ((hx >> 30) & 2);
    if (iy as u32 | ly) == 0 {
        // y = 0
        return match m {
            0 | 1 => y,
            2 => PI + TINY,
            _ => -PI - TINY,
        };
    }
    if (ix as u32 | lx) == 0 {
        // x = 0
        return if hy < 0 { -PI_O_2 - TINY } else { PI_O_2 + TINY };
    }
    if ix == 0x7ff00000 {
        // x is infinite
        if iy == 0x7ff00000 {
            return match m {
                0 => PI_O_4 + TINY,
                1 => -PI_O_4 - TINY,
                2 => 3.0 * PI_O_4 + TINY,
                _ => -3.0 * PI_O_4 - TINY,
            };
        }
        return match m {
            0 => 0.0,
            1 => -0.0,
            2 => PI + TINY,
            _ => -PI - TINY,
        };
    }
    if iy == 0x7ff00000 {
        // y is infinite
        return if hy < 0 { -PI_O_2 - TINY } else { PI_O_2 + TINY };
    }
    let k = (iy - ix) >> 20;
    let z = if k > 60 {
        // |y/x| > 2^60
        PI_O_2 + 0.5 * PI_LO
    } else if hx < 0 && k < -60 {
        // |y|/x < -2^60
        0.0
    } else {
        atan((y / x).abs())
    };
    match m {
        0 => z,
        1 => with_hi(z, hx_neg(z)),
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}

#[inline(always)]
fn hx_neg(z: f64) -> i32 {
    hi(z) ^ (0x80000000_u32 as i32)
}

// ---- exp, log, pow ----

const P1: f64 = 1.66666666666666019037e-01;
const P2: f64 = -2.77777777770155933842e-03;
const P3: f64 = 6.61375632143793436117e-05;
const P4: f64 = -1.65339022054652515390e-06;
const P5: f64 = 4.13813679705723846039e-08;

/// `exp`, as V8 has it: fdlibm's with FreeBSD's scaling of the result,
/// and `exp(1)` returning `E` exactly (the computation gets its last bit
/// wrong, which V8 special-cases).
pub fn exp(x: f64) -> f64 {
    const HALF: [f64; 2] = [0.5, -0.5];
    const TWOM1000: f64 = 9.33263618503218878990e-302;
    const TWO1023: f64 = 8.988465674311579539e307;
    const E: f64 = 2.718281828459045;
    const O_THRESHOLD: f64 = 7.09782712893383973096e+02;
    const U_THRESHOLD: f64 = -7.45133219101941108420e+02;
    const LN2HI: [f64; 2] = [6.93147180369123816490e-01, -6.93147180369123816490e-01];
    const LN2LO: [f64; 2] = [1.90821492927058770002e-10, -1.90821492927058770002e-10];
    const INVLN2: f64 = 1.44269504088896338700e+00;
    let mut x = x;
    let mut hx = hi(x);
    let xsb = ((hx >> 31) & 1) as usize;
    hx &= 0x7fffffff;
    if hx >= 0x40862E42 {
        // |x| >= 709.78...
        if hx >= 0x7ff00000 {
            if ((hx & 0xfffff) as u32 | lo(x)) != 0 {
                return x + x; // NaN
            }
            return if xsb == 0 { x } else { 0.0 }; // exp(+-inf) = inf, 0
        }
        if x > O_THRESHOLD {
            return HUGE * HUGE;
        }
        if x < U_THRESHOLD {
            return TWOM1000 * TWOM1000;
        }
    }
    let (k, hi_, lo_);
    if hx > 0x3fd62e42 {
        // |x| > 0.5 ln2
        if hx < 0x3FF0A2B2 {
            // and |x| < 1.5 ln2
            if x == 1.0 {
                return E;
            }
            hi_ = x - LN2HI[xsb];
            lo_ = LN2LO[xsb];
            k = 1 - xsb as i32 - xsb as i32;
        } else {
            k = (INVLN2 * x + HALF[xsb]) as i32;
            let t = k as f64;
            hi_ = x - t * LN2HI[0];
            lo_ = t * LN2LO[0];
        }
        x = hi_ - lo_;
    } else if hx < 0x3e300000 {
        // |x| < 2^-28
        return 1.0 + x;
    } else {
        k = 0;
        hi_ = 0.0;
        lo_ = 0.0;
    }
    let t = x * x;
    let twopk = if k >= -1021 {
        from_words(0x3ff00000 + (k << 20), 0)
    } else {
        from_words(0x3ff00000 + ((k + 1000) << 20), 0)
    };
    let c = x - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    if k == 0 {
        return 1.0 - ((x * c) / (c - 2.0) - x);
    }
    let y = 1.0 - ((lo_ - (x * c) / (2.0 - c)) - hi_);
    if k >= -1021 {
        if k == 1024 {
            return y * 2.0 * TWO1023;
        }
        y * twopk
    } else {
        y * twopk * TWOM1000
    }
}

pub fn log(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const LG1: f64 = 6.666666666666735130e-01;
    const LG2: f64 = 3.999999999940941908e-01;
    const LG3: f64 = 2.857142874366239149e-01;
    const LG4: f64 = 2.222219843214978396e-01;
    const LG5: f64 = 1.818357216161805012e-01;
    const LG6: f64 = 1.531383769920937332e-01;
    const LG7: f64 = 1.479819860511658591e-01;
    let mut x = x;
    let mut hx = hi(x);
    let lx = lo(x);
    let mut k = 0;
    if hx < 0x00100000 {
        // x < 2^-1022
        if ((hx & 0x7fffffff) as u32 | lx) == 0 {
            return f64::NEG_INFINITY; // log(+-0) = -inf
        }
        if hx < 0 {
            return (x - x) / 0.0; // log(-#) = NaN
        }
        k -= 54;
        x *= TWO54;
        hx = hi(x);
    }
    if hx >= 0x7ff00000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000fffff;
    let i = (hx + 0x95f64) & 0x100000;
    x = with_hi(x, hx | (i ^ 0x3ff00000)); // normalize x or x/2
    k += i >> 20;
    let f = x - 1.0;
    if (0x000fffff & (2 + hx)) < 3 {
        // |f| < 2^-20
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            let dk = k as f64;
            return dk * LN2_HI + dk * LN2_LO;
        }
        let r = f * f * (0.5 - 0.33333333333333333 * f);
        if k == 0 {
            return f - r;
        }
        let dk = k as f64;
        return dk * LN2_HI - ((r - dk * LN2_LO) - f);
    }
    let s = f / (2.0 + f);
    let dk = k as f64;
    let z = s * s;
    let i = hx - 0x6147a;
    let w = z * z;
    let j = 0x6b851 - hx;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    let i = i | j;
    let r = t2 + t1;
    if i > 0 {
        let hfsq = 0.5 * f * f;
        if k == 0 {
            f - (hfsq - s * (hfsq + r))
        } else {
            dk * LN2_HI - ((hfsq - (s * (hfsq + r) + dk * LN2_LO)) - f)
        }
    } else if k == 0 {
        f - s * (f - r)
    } else {
        dk * LN2_HI - ((s * (f - r) - dk * LN2_LO) - f)
    }
}

pub fn pow(x: f64, y: f64) -> f64 {
    const BP: [f64; 2] = [1.0, 1.5];
    const DP_H: [f64; 2] = [0.0, 5.84962487220764160156e-01];
    const DP_L: [f64; 2] = [0.0, 1.35003920212974897128e-08];
    const TWO53: f64 = 9007199254740992.0;
    const L1: f64 = 5.99999999999994648725e-01;
    const L2: f64 = 4.28571428578550184252e-01;
    const L3: f64 = 3.33333329818377432918e-01;
    const L4: f64 = 2.72728123808534006489e-01;
    const L5: f64 = 2.30660745775561754067e-01;
    const L6: f64 = 2.06975017800338417784e-01;
    const LG2: f64 = 6.93147180559945286227e-01;
    const LG2_H: f64 = 6.93147182464599609375e-01;
    const LG2_L: f64 = -1.90465429995776804525e-09;
    const OVT: f64 = 8.0085662595372944372e-0017;
    const CP: f64 = 9.61796693925975554329e-01;
    const CP_H: f64 = 9.61796700954437255859e-01;
    const CP_L: f64 = -7.02846165095275826516e-09;
    const IVLN2: f64 = 1.44269504088896338700e+00;
    const IVLN2_H: f64 = 1.44269502162933349609e+00;
    const IVLN2_L: f64 = 1.92596299112661746887e-08;

    let hx = hi(x);
    let lx = lo(x);
    let hy = hi(y);
    let ly = lo(y);
    let mut ix = hx & 0x7fffffff;
    let iy = hy & 0x7fffffff;

    // y == 0: x**0 = 1
    if (iy as u32 | ly) == 0 {
        return 1.0;
    }
    // +-NaN
    if ix > 0x7ff00000
        || (ix == 0x7ff00000 && lx != 0)
        || iy > 0x7ff00000
        || (iy == 0x7ff00000 && ly != 0)
    {
        return x + y;
    }
    // When x < 0, is y an integer? 0: no; 1: odd; 2: even.
    let mut yisint = 0;
    if hx < 0 {
        if iy >= 0x43400000 {
            yisint = 2; // even integer y
        } else if iy >= 0x3ff00000 {
            let k = (iy >> 20) - 0x3ff; // exponent
            if k > 20 {
                let j = ly >> (52 - k);
                if (j << (52 - k)) == ly {
                    yisint = 2 - (j & 1) as i32;
                }
            } else if ly == 0 {
                let j = iy >> (20 - k);
                if (j << (20 - k)) == iy {
                    yisint = 2 - (j & 1);
                }
            }
        }
    }
    // Special values of y.
    if ly == 0 {
        if iy == 0x7ff00000 {
            // y is +-inf
            return if ((ix - 0x3ff00000) as u32 | lx) == 0 {
                y - y // +-1**+-inf is NaN
            } else if ix >= 0x3ff00000 {
                // (|x|>1)**+-inf = inf, 0
                if hy >= 0 { y } else { 0.0 }
            } else {
                // (|x|<1)**-,+inf = inf, 0
                if hy < 0 { -y } else { 0.0 }
            };
        }
        if iy == 0x3ff00000 {
            // y is +-1
            return if hy < 0 { 1.0 / x } else { x };
        }
        if hy == 0x40000000 {
            return x * x; // y is 2
        }
        if hy == 0x3fe00000 && hx >= 0 {
            // y is 0.5, x >= +0
            return x.sqrt();
        }
    }

    let mut ax = x.abs();
    // Special values of x.
    if lx == 0 && (ix == 0x7ff00000 || ix == 0 || ix == 0x3ff00000) {
        let mut z = ax; // x is +-0, +-inf, +-1
        if hy < 0 {
            z = 1.0 / z; // z = (1/|x|)
        }
        if hx < 0 {
            if ((ix - 0x3ff00000) | yisint) == 0 {
                z = (z - z) / (z - z); // (-1)**non-int is NaN
            } else if yisint == 1 {
                z = -z; // (x<0)**odd = -(|x|**odd)
            }
        }
        return z;
    }

    let mut n = (hx >> 31) + 1;
    // (x<0)**(non-int) is NaN
    if (n | yisint) == 0 {
        return (x - x) / (x - x);
    }
    let mut s = 1.0; // sign of the result: -1 for (-ve)**(odd int)
    if (n | (yisint - 1)) == 0 {
        s = -1.0;
    }

    let (t1, t2);
    if iy > 0x41e00000 {
        // |y| > 2^31
        if iy > 0x43f00000 {
            // |y| > 2^64: must over/underflow
            if ix <= 0x3fefffff {
                return if hy < 0 { HUGE * HUGE } else { TINY * TINY };
            }
            if ix >= 0x3ff00000 {
                return if hy > 0 { HUGE * HUGE } else { TINY * TINY };
            }
        }
        // Over/underflow if x is not close to one.
        if ix < 0x3fefffff {
            return if hy < 0 { s * HUGE * HUGE } else { s * TINY * TINY };
        }
        if ix > 0x3ff00000 {
            return if hy > 0 { s * HUGE * HUGE } else { s * TINY * TINY };
        }
        // |1-x| is tiny <= 2^-20: log(x) by x - x^2/2 + x^3/3 - x^4/4
        let t = ax - 1.0;
        let w = (t * t) * (0.5 - t * (0.3333333333333333333333 - t * 0.25));
        let u = IVLN2_H * t;
        let v = t * IVLN2_L - w * IVLN2;
        t1 = with_lo(u + v, 0);
        t2 = v - (t1 - u);
    } else {
        n = 0;
        // Subnormal x.
        if ix < 0x00100000 {
            ax *= TWO53;
            n -= 53;
            ix = hi(ax);
        }
        n += (ix >> 20) - 0x3ff;
        let j = ix & 0x000fffff;
        // The interval.
        ix = j | 0x3ff00000; // normalize ix
        let k: usize;
        if j <= 0x3988E {
            k = 0; // |x| < sqrt(3/2)
        } else if j < 0xBB67A {
            k = 1; // |x| < sqrt(3)
        } else {
            k = 0;
            n += 1;
            ix -= 0x00100000;
        }
        ax = with_hi(ax, ix);
        // ss = s_h + s_l = (x-1)/(x+1) or (x-1.5)/(x+1.5)
        let u = ax - BP[k];
        let v = 1.0 / (ax + BP[k]);
        let ss = u * v;
        let s_h = with_lo(ss, 0);
        // t_h = ax + bp[k], high
        let t_h = from_words(((ix >> 1) | 0x20000000) + 0x00080000 + ((k as i32) << 18), 0);
        let t_l = ax - (t_h - BP[k]);
        let s_l = v * ((u - s_h * t_h) - s_h * t_l);
        // log(ax)
        let s2 = ss * ss;
        let mut r = s2 * s2 * (L1 + s2 * (L2 + s2 * (L3 + s2 * (L4 + s2 * (L5 + s2 * L6)))));
        r += s_l * (s_h + ss);
        let s2 = s_h * s_h;
        let t_h = with_lo(3.0 + s2 + r, 0);
        let t_l = r - ((t_h - 3.0) - s2);
        // u + v = ss*(1+...)
        let u = s_h * t_h;
        let v = s_l * t_h + t_l * ss;
        // 2/(3log2)*(ss+...)
        let p_h = with_lo(u + v, 0);
        let p_l = v - (p_h - u);
        let z_h = CP_H * p_h;
        let z_l = CP_L * p_h + p_l * CP + DP_L[k];
        // log2(ax) = (ss+..)*2/(3*log2) = n + dp_h + z_h + z_l
        let t = n as f64;
        t1 = with_lo(((z_h + z_l) + DP_H[k]) + t, 0);
        t2 = z_l - (((t1 - t) - DP_H[k]) - z_h);
    }

    // y as y1 + y2; (y1+y2)*(t1+t2)
    let y1 = with_lo(y, 0);
    let p_l = (y - y1) * t1 + y * t2;
    let mut p_h = y1 * t1;
    let z = p_l + p_h;
    let j = hi(z);
    let i = lo(z);
    if j >= 0x40900000 {
        // z >= 1024
        if ((j - 0x40900000) as u32 | i) != 0 || p_l + OVT > z - p_h {
            return s * HUGE * HUGE; // overflow
        }
    } else if (j & 0x7fffffff) >= 0x4090cc00 {
        // z <= -1075
        if ((j as u32).wrapping_sub(0xc090cc00) | i) != 0 || p_l <= z - p_h {
            return s * TINY * TINY; // underflow
        }
    }
    // 2**(p_h+p_l)
    let i = j & 0x7fffffff;
    let mut k = (i >> 20) - 0x3ff;
    let mut n = 0;
    if i > 0x3fe00000 {
        // |z| > 0.5: n = [z+0.5]
        n = j + (0x00100000 >> (k + 1));
        k = ((n & 0x7fffffff) >> 20) - 0x3ff; // new k for n
        let t = from_words(n & !(0x000fffff >> k), 0);
        n = ((n & 0x000fffff) | 0x00100000) >> (20 - k);
        if j < 0 {
            n = -n;
        }
        p_h -= t;
    }
    let t = with_lo(p_l + p_h, 0);
    let u = t * LG2_H;
    let v = (p_l - (t - p_h)) * LG2 + t * LG2_L;
    let z = u + v;
    let w = v - (z - u);
    let t = z * z;
    let t1 = z - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    let r = (z * t1) / (t1 - 2.0) - (w + z * w);
    let z = 1.0 - (r - z);
    let j = hi(z) + (n << 20);
    if (j >> 20) <= 0 {
        s * scalbn(z, n) // subnormal output
    } else {
        s * with_hi(z, j)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Node's (V8's) results, as bits, on x86-64: every reduction path of
    /// the trigonometry (none, +-1, medium, large), the tie in `exp(1)`,
    /// the subnormal ends of `exp`, `log` and `pow`, and `pow`'s sign and
    /// overflow cases.
    #[test]
    fn bit_for_bit_with_v8() {
        let b = f64::to_bits;
        assert_eq!(b(tan(1.0)), 0x3ff8eb245cbee3a6);
        assert_eq!(b(asin(0.5)), 0x3fe0c152382d7366);
        assert_eq!(b(acos(0.5)), 0x3ff0c152382d7366);
        assert_eq!(b(atan(3.0)), 0x3ff3fc176b7a8560);
        assert_eq!(b(atan(1e5)), 0x3ff921f0d7e968a8);
        assert_eq!(b(atan2(1.0, -1.0)), 0x4002d97c7f3321d2);
        assert_eq!(b(sin(1e5)), 0x3fa24daa9c527e96);
        assert_eq!(b(cos(1e5)), 0xbfeffac3841b3da7);
        assert_eq!(b(tan(1e5)), 0xbfa250a9d503313d);
        assert_eq!(b(sin(823549.0)), 0xbfe3bc41ec71c06d);
        assert_eq!(b(cos(823549.0)), 0x3fe930880dddf826);
        assert_eq!(b(tan(823549.0)), 0xbfe91238517e869a);
        assert_eq!(b(sin(1e6)), 0xbfd6664b2568d867);
        assert_eq!(b(cos(1e6)), 0x3fedf9df9906d32c);
        assert_eq!(b(tan(1e6)), 0xbfd7e9768ab734c0);
        assert_eq!(b(sin(1e10)), 0xbfdf334c7896a4e3);
        assert_eq!(b(cos(1e10)), 0x3febf098901c931a);
        assert_eq!(b(tan(1e10)), 0xbfe1de000f443f50);
        assert_eq!(b(sin(1e22)), 0xbfeb453ab76bf397);
        assert_eq!(b(cos(1e22)), 0x3fe0be2cef01c8f4);
        assert_eq!(b(sin(1e300)), 0xbfea2c16b010e385);
        assert_eq!(b(cos(1e300)), 0xbfe2699022adc4c1);
        assert_eq!(b(exp(1.0)), 0x4005bf0a8b145769);
        assert_eq!(b(exp(0.5)), 0x3ffa61298e1e069c);
        assert_eq!(b(exp(100.0)), 0x48f3494a9b171bf5);
        assert_eq!(b(exp(709.782712893384)), 0x7fefffffffffff2a);
        assert_eq!(b(exp(-745.1332191019411)), 0x0000000000000001);
        assert_eq!(b(log(10.0)), 0x40026bb1bbb55516);
        assert_eq!(b(log(1e300)), 0x4085963447f87fb5);
        assert_eq!(b(log(0.7071)), 0xbfd62e6b3842a25e);
        assert_eq!(b(log(5e-324)), 0xc0874385446d71c3);
        assert_eq!(b(pow(2.0, 0.5)), 0x3ff6a09e667f3bcd);
        assert_eq!(b(pow(-2.0, 3.0)), 0xc020000000000000);
        assert_eq!(b(pow(2.0, -1074.0)), 0x0000000000000001);
        assert_eq!(b(pow(1.0000001, 1e12)), 0x7ff0000000000000);
        assert!(pow(1.0, f64::NAN).is_nan());
        assert!(pow(-1.0, f64::INFINITY).is_nan());
        assert!(pow(-0.5, 0.5).is_nan());
        assert_eq!(pow(f64::NAN, 0.0), 1.0);
        assert_eq!(b(pow(-0.0, -3.0)), b(f64::NEG_INFINITY));
        assert_eq!(b(atan2(-0.0, -1.0)), b(-std::f64::consts::PI));
    }
}

/// `d` rounded to the nearest IEEE binary16 (ties to even), as its bits:
/// what a `Float16Array` stores and `Math.f16round` rounds to. NaN is the
/// canonical quiet NaN.
pub fn f16_bits(d: f64) -> u16 {
    if d.is_nan() {
        return 0x7E00;
    }
    let sign = if d.is_sign_negative() { 0x8000 } else { 0 };
    let a = d.abs();
    // Below 2^-14, subnormals: multiples of 2^-24 (1024 is the least
    // normal, which its bits are too).
    if a < 2f64.powi(-14) {
        return sign | (a * 2f64.powi(24)).round_ties_even() as u16;
    }
    // `a` = 2^`e` * `m`, `m` in [1, 2): 10 bits of `m` kept. (Each step is
    // exact: scaling by a power of two, and `m - 1` of a double in [1, 2).)
    let mut e = ((a.to_bits() >> 52) as i32) - 1023;
    let mut frac = ((a / 2f64.powi(e) - 1.0) * 1024.0).round_ties_even() as u16;
    if frac == 1024 {
        frac = 0;
        e += 1;
    }
    if e > 15 {
        return sign | 0x7C00;
    }
    sign | ((e + 15) as u16) << 10 | frac
}

/// The binary16 of bits `h`, as a double (exactly).
pub fn f16_value(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let (e, m) = ((h >> 10) & 0x1F, (h & 0x3FF) as f64);
    sign * match e {
        0 => m * 2f64.powi(-24),
        31 if m == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        e => (1024.0 + m) * 2f64.powi(e as i32 - 25),
    }
}
