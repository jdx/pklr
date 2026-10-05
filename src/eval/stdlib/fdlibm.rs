//! Bit-exact Rust port of fdlibm 5.3's permissively licensed Sun/FreeBSD
//! algorithms, matching Java `StrictMath`.
//!
//! Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//! Permission to use, copy, modify, and distribute this software is freely
//! granted, provided that this notice is preserved.
//!
//! Pkl's `math` module is implemented with `java.lang.StrictMath`, whose
//! transcendental functions are specified to return exactly the fdlibm
//! results. The platform libm gives slightly different last-bit results on
//! some inputs, so these ports exist to reproduce pkl's output exactly.
//!
//! Java's 32-bit `int` arithmetic wraps; it is mirrored here with `i32`
//! and explicit wrapping operations where overflow is possible.
//!
//! Floating-point constants are written as the shortest decimal that
//! round-trips to the original fdlibm hex constant (shown in the comment).

// fdlibm writes `x - x` and `(x - x) / (x - x)` on purpose to produce NaN
// from the argument, and keeps its constants as published.
#![allow(
    clippy::approx_constant,
    clippy::eq_op,
    clippy::excessive_precision,
    clippy::unreadable_literal
)]

// ---------------------------------------------------------------------------
// Bit-manipulation helpers (Java's __HI / __LO)
// ---------------------------------------------------------------------------

/// High-order 32 bits of `x` as a signed int.
#[inline]
fn hi(x: f64) -> i32 {
    (x.to_bits() >> 32) as i32
}

/// Low-order 32 bits of `x` as a signed int.
#[inline]
fn lo(x: f64) -> i32 {
    x.to_bits() as i32
}

/// `x` with its high word replaced by `high`.
#[inline]
fn with_hi(x: f64, high: i32) -> f64 {
    f64::from_bits((x.to_bits() & 0xFFFF_FFFF) | (u64::from(high as u32) << 32))
}

/// `x` with its low word replaced by `low`.
#[inline]
fn with_lo(x: f64, low: i32) -> f64 {
    f64::from_bits((x.to_bits() & 0xFFFF_FFFF_0000_0000) | u64::from(low as u32))
}

/// A double built from a high and a low word.
#[inline]
fn from_hi_lo(high: i32, low: i32) -> f64 {
    f64::from_bits((u64::from(high as u32) << 32) | u64::from(low as u32))
}

const TWO24: f64 = 16777216.0; // 0x1.0p24
const TWO54: f64 = 1.8014398509481984e+16; // 0x1.0p54
const HUGE: f64 = 1.0e+300;

const EXP_BITS: i32 = 0x7ff0_0000;
const EXP_SIGNIF_BITS: i32 = 0x7fff_ffff;

/// Port of `java.lang.Math.scalb(double, int)`: `d * 2^n` with at most one
/// rounding, using the same multiplication order as Java.
fn scalb(mut d: f64, scale_factor: i32) -> f64 {
    // MAX_EXPONENT + -MIN_EXPONENT + SIGNIFICAND_WIDTH + 1
    const MAX_SCALE: i32 = 1023 + 1022 + 53 + 1;
    let mut sf;
    let scale_increment;
    let exp_delta;
    if scale_factor < 0 {
        sf = scale_factor.max(-MAX_SCALE);
        scale_increment = -512;
        exp_delta = 7.458340731200207e-155;
    } else {
        sf = scale_factor.min(MAX_SCALE);
        scale_increment = 512;
        exp_delta = 1.3407807929942597e+154;
    }
    // sf % +/-512 (Hacker's Delight 10-2).
    let t = ((sf >> 8) as u32 >> 23) as i32;
    let exp_adjust = ((sf + t) & 511) - t;
    d *= f64::from_bits(((exp_adjust + 1023) as u64) << 52);
    sf -= exp_adjust;
    while sf != 0 {
        d *= exp_delta;
        sf -= scale_increment;
    }
    d
}

// ---------------------------------------------------------------------------
// sin / cos / tan
// ---------------------------------------------------------------------------

/// `StrictMath.sin`.
pub(crate) fn sin(x: f64) -> f64 {
    let ix = hi(x) & EXP_SIGNIF_BITS;
    if ix <= 0x3fe9_21fb {
        // |x| ~< pi/4
        kernel_sin(x, 0.0, 0)
    } else if ix >= EXP_BITS {
        // sin(Inf or NaN) is NaN
        x - x
    } else {
        let (n, y) = rem_pio2(x);
        match n & 3 {
            0 => kernel_sin(y[0], y[1], 1),
            1 => kernel_cos(y[0], y[1]),
            2 => -kernel_sin(y[0], y[1], 1),
            _ => -kernel_cos(y[0], y[1]),
        }
    }
}

/// `StrictMath.cos`.
pub(crate) fn cos(x: f64) -> f64 {
    let ix = hi(x) & EXP_SIGNIF_BITS;
    if ix <= 0x3fe9_21fb {
        kernel_cos(x, 0.0)
    } else if ix >= EXP_BITS {
        x - x
    } else {
        let (n, y) = rem_pio2(x);
        match n & 3 {
            0 => kernel_cos(y[0], y[1]),
            1 => -kernel_sin(y[0], y[1], 1),
            2 => -kernel_cos(y[0], y[1]),
            _ => kernel_sin(y[0], y[1], 1),
        }
    }
}

/// `StrictMath.tan`.
pub(crate) fn tan(x: f64) -> f64 {
    let ix = hi(x) & EXP_SIGNIF_BITS;
    if ix <= 0x3fe9_21fb {
        kernel_tan(x, 0.0, 1)
    } else if ix >= EXP_BITS {
        x - x
    } else {
        let (n, y) = rem_pio2(x);
        // 1 -- n even; -1 -- n odd
        kernel_tan(y[0], y[1], 1 - ((n & 1) << 1))
    }
}

/// Kernel sin on [-pi/4, pi/4]. `y` is the tail of `x`; `iy == 0` means `y`
/// is zero.
///
/// sin(x) ~ x + S1*x^3 + ... + S6*x^13; with r = x^3*(S2+x^2*(S3+...)),
/// sin(x+y) = x + (S1*x^3 + (x^2*(r-y/2)+y)).
fn kernel_sin(x: f64, y: f64, iy: i32) -> f64 {
    const S1: f64 = -0.16666666666666632; // 0x1.5555555555549p-3
    const S2: f64 = 0.00833333333332249; // 0x1.111111110f8a6p-7
    const S3: f64 = -0.0001984126982985795; // 0x1.a01a019c161d5p-13
    const S4: f64 = 2.7557313707070068e-06; // 0x1.71de357b1fe7dp-19
    const S5: f64 = -2.5050760253406863e-08; // 0x1.ae5e68a2b9cebp-26
    const S6: f64 = 1.58969099521155e-10; // 0x1.5d93a5acfd57cp-33

    let ix = hi(x) & EXP_SIGNIF_BITS;
    if ix < 0x3e40_0000 && x as i32 == 0 {
        // |x| < 2**-27
        return x;
    }
    let z = x * x;
    let v = z * x;
    let r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
    if iy == 0 {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (0.5 * y - v * r) - y) - v * S1)
    }
}

/// Kernel cos on [-pi/4, pi/4]. `y` is the tail of `x`.
///
/// cos(x) ~ 1 - x^2/2 + C1*x^4 + ... + C6*x^14. For |x| > 0.3 the result is
/// computed as (1-qx) - ((x*x/2-qx) - (r-x*y)) with an exact qx.
fn kernel_cos(x: f64, y: f64) -> f64 {
    const C1: f64 = 0.0416666666666666; // 0x1.555555555554cp-5
    const C2: f64 = -0.001388888888887411; // 0x1.6c16c16c15177p-10
    const C3: f64 = 2.480158728947673e-05; // 0x1.a01a019cb159p-16
    const C4: f64 = -2.7557314351390663e-07; // 0x1.27e4f809c52adp-22
    const C5: f64 = 2.087572321298175e-09; // 0x1.1ee9ebdb4b1c4p-29
    const C6: f64 = -1.1359647557788195e-11; // 0x1.8fae9be8838d4p-37

    let ix = hi(x) & EXP_SIGNIF_BITS;
    if ix < 0x3e40_0000 && x as i32 == 0 {
        // |x| < 2**-27
        return 1.0;
    }
    let z = x * x;
    let r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
    if ix < 0x3FD3_3333 {
        // |x| < 0.3
        1.0 - (0.5 * z - (z * r - x * y))
    } else {
        let qx = if ix > 0x3fe9_0000 {
            // x > 0.78125
            0.28125
        } else {
            from_hi_lo(ix - 0x0020_0000, 0)
        };
        let hz = 0.5 * z - qx;
        let a = 1.0 - qx;
        a - (hz - (z * r - x * y))
    }
}

/// Kernel tan on [-pi/4, pi/4]. `y` is the tail of `x`; returns tan(x+y)
/// when `iy == 1` and -1/tan(x+y) when `iy == -1`.
fn kernel_tan(mut x: f64, mut y: f64, iy: i32) -> f64 {
    const PIO4: f64 = 0.7853981633974483; // 0x1.921fb54442d18p-1
    const PIO4LO: f64 = 3.061616997868383e-17; // 0x1.1a62633145c07p-55
    const T: [f64; 13] = [
        0.3333333333333341,      // 0x1.5555555555563p-2
        0.13333333333320124,     // 0x1.111111110fe7ap-3
        0.05396825397622605,     // 0x1.ba1ba1bb341fep-5
        0.021869488294859542,    // 0x1.664f48406d637p-6
        0.0088632398235993,      // 0x1.226e3e96e8493p-7
        0.0035920791075913124,   // 0x1.d6d22c9560328p-9
        0.0014562094543252903,   // 0x1.7dbc8fee08315p-10
        0.0005880412408202641,   // 0x1.344d8f2f26501p-11
        0.0002464631348184699,   // 0x1.026f71a8d1068p-12
        7.817944429395571e-05,   // 0x1.47e88a03792a6p-14
        7.140724913826082e-05,   // 0x1.2b80f32f0a7e9p-14
        -1.8558637485527546e-05, // 0x1.375cbdb605373p-16
        2.590730518636337e-05,   // 0x1.b2a7074bf7ad4p-16
    ];

    let hx = hi(x);
    let ix = hx & EXP_SIGNIF_BITS;
    if ix < 0x3e30_0000 && x as i32 == 0 {
        // |x| < 2**-28
        if ((ix | lo(x)) | (iy + 1)) == 0 {
            return 1.0 / x.abs();
        } else if iy == 1 {
            return x;
        } else {
            // compute -1 / (x+y) carefully
            let w = x + y;
            let z = with_lo(w, 0);
            let v = y - (z - x);
            let a = -1.0 / w;
            let t = with_lo(a, 0);
            let s = 1.0 + t * z;
            return t + a * (s + t * v);
        }
    }
    if ix >= 0x3FE5_9428 {
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
    // Break x^5*(T[1]+x^2*T[2]+...) into
    //   x^5(T[1]+x^4*T[3]+...+x^20*T[11]) +
    //   x^5(x^2*(T[2]+x^4*T[4]+...+x^22*[T12]))
    let mut r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    let v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    let s = z * x;
    r = y + z * (s * (r + v) + y);
    r += T[0] * s;
    let w = x + r;
    if ix >= 0x3FE5_9428 {
        let v = f64::from(iy);
        return f64::from(1 - ((hx >> 30) & 2)) * (v - 2.0 * (x - (w * w / (w + v) - r)));
    }
    if iy == 1 {
        w
    } else {
        // compute -1.0/(x + r) accurately
        let z = with_lo(w, 0);
        let v = r - (z - x); // z + v = r + x
        let a = -1.0 / w;
        let t = with_lo(a, 0);
        let s = 1.0 + t * z;
        t + a * (s + t * v)
    }
}

// ---------------------------------------------------------------------------
// Argument reduction
// ---------------------------------------------------------------------------

/// 396 hex digits (476 decimal) of 2/pi, in 24-bit chunks.
const TWO_OVER_PI: [i32; 66] = [
    0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
    0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
    0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
    0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
    0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
    0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
    0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
    0x73A8C9, 0x60E27B, 0xC08C6B,
];

const NPIO2_HW: [i32; 32] = [
    0x3FF921FB, 0x400921FB, 0x4012D97C, 0x401921FB, 0x401F6A7A, 0x4022D97C, 0x4025FDBB, 0x402921FB,
    0x402C463A, 0x402F6A7A, 0x4031475C, 0x4032D97C, 0x40346B9C, 0x4035FDBB, 0x40378FDB, 0x403921FB,
    0x403AB41B, 0x403C463A, 0x403DD85A, 0x403F6A7A, 0x40407E4C, 0x4041475C, 0x4042106C, 0x4042D97C,
    0x4043A28C, 0x40446B9C, 0x404534AC, 0x4045FDBB, 0x4046C6CB, 0x40478FDB, 0x404858EB, 0x404921FB,
];

/// `__ieee754_rem_pio2`: returns `n` and `y[0] + y[1] = x - n*pi/2`.
fn rem_pio2(x: f64) -> (i32, [f64; 2]) {
    const INVPIO2: f64 = 0.6366197723675814; // 0x1.45f306dc9c883p-1, 53 bits of 2/pi
    const PIO2_1: f64 = 1.5707963267341256; // 0x1.921fb544p0, first 33 bits of pi/2
    const PIO2_1T: f64 = 6.077100506506192e-11; // 0x1.0b4611a626331p-34, pi/2 - PIO2_1
    const PIO2_2: f64 = 6.077100506303966e-11; // 0x1.0b4611a6p-34, second 33 bits of pi/2
    const PIO2_2T: f64 = 2.0222662487959506e-21; // 0x1.3198a2e037073p-69, pi/2 - (PIO2_1+PIO2_2)
    const PIO2_3: f64 = 2.0222662487111665e-21; // 0x1.3198a2ep-69, third 33 bits of pi/2
    const PIO2_3T: f64 = 8.4784276603689e-32; // 0x1.b839a252049c1p-104, pi/2 - (PIO2_1+PIO2_2+PIO2_3)

    let mut y = [0.0f64; 2];
    let hx = hi(x);
    let ix = hx & EXP_SIGNIF_BITS;
    if ix <= 0x3fe9_21fb {
        // |x| ~<= pi/4, no need for reduction
        y[0] = x;
        y[1] = 0.0;
        return (0, y);
    }
    if ix < 0x4002_d97c {
        // |x| < 3pi/4, special case with n=+-1
        if hx > 0 {
            let mut z = x - PIO2_1;
            if ix != 0x3ff9_21fb {
                // 33+53 bit pi is good enough
                y[0] = z - PIO2_1T;
                y[1] = (z - y[0]) - PIO2_1T;
            } else {
                // near pi/2, use 33+33+53 bit pi
                z -= PIO2_2;
                y[0] = z - PIO2_2T;
                y[1] = (z - y[0]) - PIO2_2T;
            }
            return (1, y);
        } else {
            let mut z = x + PIO2_1;
            if ix != 0x3ff9_21fb {
                y[0] = z + PIO2_1T;
                y[1] = (z - y[0]) + PIO2_1T;
            } else {
                z += PIO2_2;
                y[0] = z + PIO2_2T;
                y[1] = (z - y[0]) + PIO2_2T;
            }
            return (-1, y);
        }
    }
    if ix <= 0x4139_21fb {
        // |x| ~<= 2^19*(pi/2), medium size
        let mut t = x.abs();
        let n = (t * INVPIO2 + 0.5) as i32;
        let fn_ = f64::from(n);
        let mut r = t - fn_ * PIO2_1;
        let mut w = fn_ * PIO2_1T; // 1st round good to 85 bit
        if n < 32 && ix != NPIO2_HW[(n - 1) as usize] {
            y[0] = r - w; // quick check no cancellation
        } else {
            let j = ix >> 20;
            y[0] = r - w;
            let i = j - ((hi(y[0]) >> 20) & 0x7ff);
            if i > 16 {
                // 2nd iteration needed, good to 118
                t = r;
                w = fn_ * PIO2_2;
                r = t - w;
                w = fn_ * PIO2_2T - ((t - r) - w);
                y[0] = r - w;
                let i = j - ((hi(y[0]) >> 20) & 0x7ff);
                if i > 49 {
                    // 3rd iteration need, 151 bits acc
                    t = r;
                    w = fn_ * PIO2_3;
                    r = t - w;
                    w = fn_ * PIO2_3T - ((t - r) - w);
                    y[0] = r - w;
                }
            }
        }
        y[1] = (r - y[0]) - w;
        if hx < 0 {
            y[0] = -y[0];
            y[1] = -y[1];
            return (-n, y);
        }
        return (n, y);
    }
    // all other (large) arguments
    if ix >= EXP_BITS {
        // x is inf or NaN
        y[0] = x - x;
        y[1] = y[0];
        return (0, y);
    }
    // set z = scalbn(|x|, ilogb(x)-23)
    let mut z = from_hi_lo(0, lo(x));
    let e0 = (ix >> 20) - 1046; // e0 = ilogb(z) - 23
    z = with_hi(z, ix - (e0 << 20));
    let mut tx = [0.0f64; 3];
    for t in tx.iter_mut().take(2) {
        *t = f64::from(z as i32);
        z = (z - *t) * TWO24;
    }
    tx[2] = z;
    let mut nx = 3;
    while tx[nx - 1] == 0.0 {
        // skip zero term
        nx -= 1;
    }
    let n = kernel_rem_pio2(&tx[..nx], &mut y, e0);
    if hx < 0 {
        y[0] = -y[0];
        y[1] = -y[1];
        return (-n, y);
    }
    (n, y)
}

/// `__kernel_rem_pio2` specialized to `prec = 2` (the only precision used by
/// `rem_pio2`). `x` holds the input broken into 24-bit pieces with exponent
/// `e0` for `x[0]`. Returns the last three bits of `N` with `y = x - N*pi/2`.
fn kernel_rem_pio2(x: &[f64], y: &mut [f64; 2], e0: i32) -> i32 {
    // pi/2 cut into 24-bit chunks.
    const PIO2: [f64; 8] = [
        1.570796251296997,      // 0x1.921fb4p0
        7.549789415861596e-08,  // 0x1.4442dp-24
        5.390302529957765e-15,  // 0x1.846988p-48
        3.282003415807913e-22,  // 0x1.8cc516p-72
        1.270655753080676e-29,  // 0x1.01b838p-96
        1.2293330898111133e-36, // 0x1.a25204p-120
        2.7337005381646456e-44, // 0x1.382228p-145
        2.1674168387780482e-51, // 0x1.9f31dp-169
    ];
    const TWON24: f64 = 5.960464477539063e-08; // 0x1.0p-24
    const JK: usize = 4; // init_jk[prec = 2]
    const JP: usize = JK;
    let ipio2 = &TWO_OVER_PI;

    let mut iq = [0i32; 20];
    let mut f = [0.0f64; 20];
    let mut fq = [0.0f64; 20];
    let mut q = [0.0f64; 20];

    // determine jx, jv, q0, note that 3 > q0
    let jx = x.len() - 1;
    let jv = ((e0 - 3) / 24).max(0);
    let mut q0 = e0 - 24 * (jv + 1);
    let jv = jv as usize;

    // set up f[0] to f[jx+jk] where f[jx+jk] = ipio2[jv+jk]
    let first = jv as isize - jx as isize;
    for (j, fi) in (first..).zip(f.iter_mut().take(jx + JK + 1)) {
        *fi = if j < 0 {
            0.0
        } else {
            f64::from(ipio2[j as usize])
        };
    }

    // compute q[0],q[1],...q[jk]
    for i in 0..=JK {
        let mut fw = 0.0;
        for j in 0..=jx {
            fw += x[j] * f[jx + i - j];
        }
        q[i] = fw;
    }

    let mut jz = JK;
    let mut z;
    let mut n;
    let mut ih;
    loop {
        // distill q[] into iq[] reversingly
        z = q[jz];
        let mut i = 0;
        let mut j = jz;
        while j > 0 {
            let fw = f64::from((TWON24 * z) as i32);
            iq[i] = (z - TWO24 * fw) as i32;
            z = q[j - 1] + fw;
            i += 1;
            j -= 1;
        }

        // compute n
        z = scalb(z, q0); // actual value of z
        z -= 8.0 * (z * 0.125).floor(); // trim off integer >= 8
        n = z as i32;
        z -= f64::from(n);
        ih = 0;
        if q0 > 0 {
            // need iq[jz-1] to determine n
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
            // q > 0.5
            n += 1;
            let mut carry = 0;
            for v in iq.iter_mut().take(jz) {
                // compute 1-q
                let j = *v;
                if carry == 0 {
                    if j != 0 {
                        carry = 1;
                        *v = 0x100_0000 - j;
                    }
                } else {
                    *v = 0xff_ffff - j;
                }
            }
            if q0 > 0 {
                // rare case: chance is 1 in 12
                match q0 {
                    1 => iq[jz - 1] &= 0x7f_ffff,
                    2 => iq[jz - 1] &= 0x3f_ffff,
                    _ => {}
                }
            }
            if ih == 2 {
                z = 1.0 - z;
                if carry != 0 {
                    z -= scalb(1.0, q0);
                }
            }
        }

        // check if recomputation is needed
        if z == 0.0 {
            let mut j = 0;
            for &v in &iq[JK..jz] {
                j |= v;
            }
            if j == 0 {
                // need recomputation
                let mut k = 1;
                while iq[JK - k] == 0 {
                    k += 1; // k = no. of terms needed
                }
                for i in jz + 1..=jz + k {
                    // add q[jz+1] to q[jz+k]
                    f[jx + i] = f64::from(ipio2[jv + i]);
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

    // chop off zero terms
    if z == 0.0 {
        jz -= 1;
        q0 -= 24;
        while iq[jz] == 0 {
            jz -= 1;
            q0 -= 24;
        }
    } else {
        // break z into 24-bit if necessary
        z = scalb(z, -q0);
        if z >= TWO24 {
            let fw = f64::from((TWON24 * z) as i32);
            iq[jz] = (z - TWO24 * fw) as i32;
            jz += 1;
            q0 += 24;
            iq[jz] = fw as i32;
        } else {
            iq[jz] = z as i32;
        }
    }

    // convert integer "bit" chunk to floating-point value
    let mut fw = scalb(1.0, q0);
    for i in (0..=jz).rev() {
        q[i] = fw * f64::from(iq[i]);
        fw *= TWON24;
    }

    // compute PIo2[0,...,jp]*q[jz,...,0]
    for i in (0..=jz).rev() {
        let mut fw = 0.0;
        let mut k = 0;
        while k <= JP && k <= jz - i {
            fw += PIO2[k] * q[i + k];
            k += 1;
        }
        fq[jz - i] = fw;
    }

    // compress fq[] into y[]
    let mut fw = 0.0;
    for i in (0..=jz).rev() {
        fw += fq[i];
    }
    y[0] = if ih == 0 { fw } else { -fw };
    fw = fq[0] - fw;
    for v in &fq[1..=jz] {
        fw += *v;
    }
    y[1] = if ih == 0 { fw } else { -fw };
    n & 7
}

// ---------------------------------------------------------------------------
// asin / acos / atan / atan2
// ---------------------------------------------------------------------------

const PIO2_HI: f64 = 1.5707963267948966; // 0x1.921fb54442d18p0
const PIO2_LO: f64 = 6.123233995736766e-17; // 0x1.1a62633145c07p-54
// Coefficients of the rational approximation R(x^2) of (asin(x)-x)/x^3.
const PS0: f64 = 0.16666666666666666; // 0x1.5555555555555p-3
const PS1: f64 = -0.3255658186224009; // 0x1.4d61203eb6f7dp-2
const PS2: f64 = 0.20121253213486293; // 0x1.9c1550e884455p-3
const PS3: f64 = -0.04005553450067941; // 0x1.48228b5688f3bp-5
const PS4: f64 = 0.0007915349942898145; // 0x1.9efe07501b288p-11
const PS5: f64 = 3.479331075960212e-05; // 0x1.23de10dfdf709p-15
const QS1: f64 = -2.403394911734414; // 0x1.33a271c8a2d4bp1
const QS2: f64 = 2.0209457602335057; // 0x1.02ae59c598ac8p1
const QS3: f64 = -0.6882839716054533; // 0x1.6066c1b8d0159p-1
const QS4: f64 = 0.07703815055590194; // 0x1.3b8c5b12e9282p-4

/// `StrictMath.asin`.
///
/// asin(x) = x + x*x^2*R(x^2) on [0, 0.5]; for x in [0.5, 1],
/// asin(x) = pi/2 - 2*asin(sqrt((1-x)/2)).
pub(crate) fn asin(x: f64) -> f64 {
    const PIO4_HI: f64 = 0.7853981633974483; // 0x1.921fb54442d18p-1

    let hx = hi(x);
    let ix = hx & EXP_SIGNIF_BITS;
    if ix >= 0x3ff0_0000 {
        // |x| >= 1
        if ((ix - 0x3ff0_0000) | lo(x)) == 0 {
            // asin(1) = +-pi/2 with inexact
            return x * PIO2_HI + x * PIO2_LO;
        }
        return (x - x) / (x - x); // asin(|x| > 1) is NaN
    } else if ix < 0x3fe0_0000 {
        // |x| < 0.5
        let mut t = 0.0;
        if ix < 0x3e40_0000 {
            // |x| < 2**-27
            if HUGE + x > 1.0 {
                return x;
            }
        } else {
            t = x * x;
        }
        let p = t * (PS0 + t * (PS1 + t * (PS2 + t * (PS3 + t * (PS4 + t * PS5)))));
        let q = 1.0 + t * (QS1 + t * (QS2 + t * (QS3 + t * QS4)));
        let w = p / q;
        return x + x * w;
    }
    // 1 > |x| >= 0.5
    let w = 1.0 - x.abs();
    let t = w * 0.5;
    let p = t * (PS0 + t * (PS1 + t * (PS2 + t * (PS3 + t * (PS4 + t * PS5)))));
    let q = 1.0 + t * (QS1 + t * (QS2 + t * (QS3 + t * QS4)));
    let s = t.sqrt();
    let t = if ix >= 0x3FEF_3333 {
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

/// `StrictMath.acos`.
///
/// acos(x) = pi/2 - asin(x), computed piecewise as in fdlibm's e_acos.c.
pub(crate) fn acos(x: f64) -> f64 {
    use std::f64::consts::PI;

    let hx = hi(x);
    let ix = hx & EXP_SIGNIF_BITS;
    if ix >= 0x3ff0_0000 {
        // |x| >= 1
        if ((ix - 0x3ff0_0000) | lo(x)) == 0 {
            // |x| == 1
            if hx > 0 {
                return 0.0; // acos(1) = 0
            } else {
                return PI + 2.0 * PIO2_LO; // acos(-1) = pi
            }
        }
        return (x - x) / (x - x); // acos(|x| > 1) is NaN
    }
    if ix < 0x3fe0_0000 {
        // |x| < 0.5
        if ix <= 0x3c60_0000 {
            // |x| < 2**-57
            return PIO2_HI + PIO2_LO;
        }
        let z = x * x;
        let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
        let q = 1.0 + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
        let r = p / q;
        PIO2_HI - (x - (PIO2_LO - x * r))
    } else if hx < 0 {
        // x < -0.5
        let z = (1.0 + x) * 0.5;
        let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
        let q = 1.0 + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
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
        let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
        let q = 1.0 + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
        let r = p / q;
        let w = r * s + c;
        2.0 * (df + w)
    }
}

/// `StrictMath.atan`.
///
/// The argument is reduced to one of [0,7/16], [7/16,11/16], [11/16,19/16],
/// [19/16,39/16], [39/16,INF] and evaluated against atan(0), atan(1/2),
/// atan(1), atan(3/2) or atan(INF) plus a polynomial correction.
pub(crate) fn atan(mut x: f64) -> f64 {
    const ATANHI: [f64; 4] = [
        0.4636476090008061, // 0x1.dac670561bb4fp-2, atan(0.5)hi
        0.7853981633974483, // 0x1.921fb54442d18p-1, atan(1.0)hi
        0.982793723247329,  // 0x1.f730bd281f69bp-1, atan(1.5)hi
        1.5707963267948966, // 0x1.921fb54442d18p0, atan(inf)hi
    ];
    const ATANLO: [f64; 4] = [
        2.2698777452961687e-17, // 0x1.a2b7f222f65e2p-56, atan(0.5)lo
        3.061616997868383e-17,  // 0x1.1a62633145c07p-55, atan(1.0)lo
        1.3903311031230998e-17, // 0x1.007887af0cbbdp-56, atan(1.5)lo
        6.123233995736766e-17,  // 0x1.1a62633145c07p-54, atan(inf)lo
    ];
    const AT: [f64; 11] = [
        0.3333333333333293,    // 0x1.555555555550dp-2
        -0.19999999999876483,  // 0x1.999999998ebc4p-3
        0.14285714272503466,   // 0x1.24924920083ffp-3
        -0.11111110405462356,  // 0x1.c71c6fe231671p-4
        0.09090887133436507,   // 0x1.745cdc54c206ep-4
        -0.0769187620504483,   // 0x1.3b0f2af749a6dp-4
        0.06661073137387531,   // 0x1.10d66a0d03d51p-4
        -0.058335701337905735, // 0x1.dde2d52defd9ap-5
        0.049768779946159324,  // 0x1.97b4b24760debp-5
        -0.036531572744216916, // 0x1.2b4442c6a6c2fp-5
        0.016285820115365782,  // 0x1.0ad3ae322da11p-6
    ];

    let hx = hi(x);
    let ix = hx & EXP_SIGNIF_BITS;
    let id: i32;
    if ix >= 0x4410_0000 {
        // |x| >= 2^66
        if ix > EXP_BITS || (ix == EXP_BITS && lo(x) != 0) {
            return x + x; // NaN
        }
        if hx > 0 {
            return ATANHI[3] + ATANLO[3];
        } else {
            return -ATANHI[3] - ATANLO[3];
        }
    }
    if ix < 0x3fdc_0000 {
        // |x| < 0.4375
        if ix < 0x3e20_0000 && HUGE + x > 1.0 {
            // |x| < 2^-29, raise inexact
            return x;
        }
        id = -1;
    } else {
        x = x.abs();
        if ix < 0x3ff3_0000 {
            // |x| < 1.1875
            if ix < 0x3fe6_0000 {
                // 7/16 <= |x| < 11/16
                id = 0;
                x = (2.0 * x - 1.0) / (2.0 + x);
            } else {
                // 11/16 <= |x| < 19/16
                id = 1;
                x = (x - 1.0) / (x + 1.0);
            }
        } else if ix < 0x4003_8000 {
            // |x| < 2.4375
            id = 2;
            x = (x - 1.5) / (1.0 + 1.5 * x);
        } else {
            // 2.4375 <= |x| < 2^66
            id = 3;
            x = -1.0 / x;
        }
    }
    // end of argument reduction
    let z = x * x;
    let w = z * z;
    // break sum from i=0 to 10 aT[i]z**(i+1) into odd and even poly
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    if id < 0 {
        x - x * (s1 + s2)
    } else {
        let id = id as usize;
        let z = ATANHI[id] - ((x * (s1 + s2) - ATANLO[id]) - x);
        if hx < 0 { -z } else { z }
    }
}

/// `StrictMath.atan2(y, x)`.
pub(crate) fn atan2(y: f64, x: f64) -> f64 {
    use std::f64::consts::PI;
    const TINY: f64 = 1.0e-300;
    const PI_O_4: f64 = 0.7853981633974483; // 0x1.921fb54442d18p-1
    const PI_O_2: f64 = 1.5707963267948966; // 0x1.921fb54442d18p0
    const PI_LO: f64 = 1.2246467991473532e-16; // 0x1.1a62633145c07p-53

    let hx = hi(x);
    let ix = hx & EXP_SIGNIF_BITS;
    let lx = lo(x);
    let hy = hi(y);
    let iy = hy & EXP_SIGNIF_BITS;
    let ly = lo(y);
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if (hx.wrapping_sub(0x3ff0_0000) | lx) == 0 {
        // x = 1.0
        return atan(y);
    }
    let m = ((hy >> 31) & 1) | ((hx >> 30) & 2); // 2*sign(x) + sign(y)

    // when y = 0
    if (iy | ly) == 0 {
        match m {
            0 | 1 => return y,      // atan(+/-0, +anything) = +/-0
            2 => return PI + TINY,  // atan(+0, -anything) = pi
            _ => return -PI - TINY, // atan(-0, -anything) = -pi
        }
    }
    // when x = 0
    if (ix | lx) == 0 {
        return if hy < 0 {
            -PI_O_2 - TINY
        } else {
            PI_O_2 + TINY
        };
    }

    // when x is INF
    if ix == EXP_BITS {
        if iy == EXP_BITS {
            return match m {
                0 => PI_O_4 + TINY,        // atan(+INF, +INF)
                1 => -PI_O_4 - TINY,       // atan(-INF, +INF)
                2 => 3.0 * PI_O_4 + TINY,  // atan(+INF, -INF)
                _ => -3.0 * PI_O_4 - TINY, // atan(-INF, -INF)
            };
        } else {
            return match m {
                0 => 0.0,        // atan(+..., +INF)
                1 => -0.0,       // atan(-..., +INF)
                2 => PI + TINY,  // atan(+..., -INF)
                _ => -PI - TINY, // atan(-..., -INF)
            };
        }
    }
    // when y is INF
    if iy == EXP_BITS {
        return if hy < 0 {
            -PI_O_2 - TINY
        } else {
            PI_O_2 + TINY
        };
    }

    // compute y/x
    let k = (iy - ix) >> 20;
    let z = if k > 60 {
        // |y/x| > 2**60
        PI_O_2 + 0.5 * PI_LO
    } else if hx < 0 && k < -60 {
        // |y|/x < -2**60
        0.0
    } else {
        // safe to do y/x
        atan((y / x).abs())
    };
    match m {
        0 => z,                // atan(+, +)
        1 => -z,               // atan(-, +)
        2 => PI - (z - PI_LO), // atan(+, -)
        _ => (z - PI_LO) - PI, // atan(-, -)
    }
}

// ---------------------------------------------------------------------------
// cbrt
// ---------------------------------------------------------------------------

/// `StrictMath.cbrt`.
pub(crate) fn cbrt(x: f64) -> f64 {
    const B1: i32 = 715094163; // (682-0.03306235651)*2**20
    const B2: i32 = 696219795; // (664-0.03306235651)*2**20
    const C: f64 = 0.5428571428571428; // 0x1.15f15f15f15f1p-1, 19/35
    const D: f64 = -0.7053061224489796; // 0x1.691de2532c834p-1, -864/1225
    const E: f64 = 1.4142857142857144; // 0x1.6a0ea0ea0ea0fp0, 99/70
    const F: f64 = 1.6071428571428572; // 0x1.9b6db6db6db6ep0, 45/28
    const G: f64 = 0.35714285714285715; // 0x1.6db6db6db6db7p-2, 5/14

    if x == 0.0 || !x.is_finite() {
        return x; // handles signed zeros properly
    }
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();

    // rough cbrt to 5 bits
    let mut t;
    if x < 2.2250738585072014e-308 {
        // subnormal number
        t = TWO54;
        t *= x;
        t = with_hi(t, hi(t) / 3 + B2);
    } else {
        t = with_hi(0.0, hi(x) / 3 + B1);
    }

    // new cbrt to 23 bits
    let r = t * t / x;
    let s = C + r * t;
    t *= G + F / (s + E + D / s);

    // chopped to 20 bits and make it larger than cbrt(x)
    t = with_lo(t, 0);
    t = with_hi(t, hi(t) + 1);

    // one step newton iteration to 53 bits with error less than 0.667 ulps
    let s = t * t; // t*t is exact
    let r = x / s;
    let w = t + t;
    let r = (r - t) / (w + r); // r-s is exact
    t += t * r;

    sign * t
}

// ---------------------------------------------------------------------------
// pow
// ---------------------------------------------------------------------------

/// `StrictMath.pow(x, y)`.
///
/// Computes log2(x) in extra precision as t1 + t2, multiplies by y split
/// into y1 + y2, and evaluates 2^(y*log2(x)) with an exp-style kernel.
pub(crate) fn pow(x: f64, y: f64) -> f64 {
    const INFINITY: f64 = f64::INFINITY;

    // y == zero: x**0 = 1
    if y == 0.0 {
        return 1.0;
    }
    // +/-NaN return x + y to propagate NaN significands
    if x.is_nan() || y.is_nan() {
        return x + y;
    }

    let y_abs = y.abs();
    let mut x_abs = x.abs();
    // Special values of y
    if y == 2.0 {
        return x * x;
    } else if y == 0.5 {
        if x >= -f64::MAX {
            // handle x == -infinity later
            return (x + 0.0).sqrt(); // add 0.0 to properly handle x == -0.0
        }
    } else if y_abs == 1.0 {
        return if y == 1.0 { x } else { 1.0 / x };
    } else if y_abs == INFINITY {
        if x_abs == 1.0 {
            return y - y; // inf**+/-1 is NaN
        } else if x_abs > 1.0 {
            // (|x| > 1)**+/-inf = inf, 0
            return if y >= 0.0 { y } else { 0.0 };
        } else {
            // (|x| < 1)**-/+inf = inf, 0
            return if y < 0.0 { -y } else { 0.0 };
        }
    }

    let hx = hi(x);
    let mut ix = hx & EXP_SIGNIF_BITS;

    // When x < 0, determine if y is an odd integer:
    // 0 ... y is not an integer, 1 ... odd int, 2 ... even int
    let mut y_is_int = 0;
    if hx < 0 {
        if y_abs >= 9007199254740992.0 {
            y_is_int = 2; // even, since ulp(2^53) = 2.0
        } else if y_abs >= 1.0 {
            let y_abs_as_long = y_abs as i64;
            if y_abs_as_long as f64 == y_abs {
                y_is_int = 2 - (y_abs_as_long & 1) as i32;
            }
        }
    }

    // Special value of x
    if x_abs == 0.0 || x_abs == INFINITY || x_abs == 1.0 {
        let mut z = x_abs; // x is +/-0, +/-inf, +/-1
        if y < 0.0 {
            z = 1.0 / z; // z = (1/|x|)
        }
        if hx < 0 {
            if ((ix - 0x3ff00000) | y_is_int) == 0 {
                z = (z - z) / (z - z); // (-1)**non-int is NaN
            } else if y_is_int == 1 {
                z = -z; // (x < 0)**odd = -(|x|**odd)
            }
        }
        return z;
    }

    let mut n = (hx >> 31) + 1;

    // (x < 0)**(non-int) is NaN
    if (n | y_is_int) == 0 {
        return (x - x) / (x - x);
    }

    // s (sign of result -ve**odd) = -1 else = 1
    let s = if (n | (y_is_int - 1)) == 0 { -1.0 } else { 1.0 };

    let (t1, t2) = if y_abs > 2147485695.9999995 {
        // |y| is huge (> ~2**31)
        const INV_LN2: f64 = 1.4426950408889634; // 0x1.71547652b82fep0, 1/ln2
        const INV_LN2_H: f64 = 1.4426950216293335; // 0x1.715476p0, 24 bits of 1/ln2
        const INV_LN2_L: f64 = 1.9259629911266175e-08; // 0x1.4ae0bf85ddf44p-26, 1/ln2 tail

        // Over/underflow if x is not close to one
        if x_abs < 0.9999995231628418 {
            return if y < 0.0 { s * INFINITY } else { s * 0.0 };
        }
        if x_abs > 1.0000009536743162 {
            return if y > 0.0 { s * INFINITY } else { s * 0.0 };
        }
        // now |1-x| is tiny <= 2**-20, sufficient to compute
        // log(x) by x - x^2/2 + x^3/3 - x^4/4
        let t = x_abs - 1.0; // t has 20 trailing zeros
        let w = (t * t) * (0.5 - t * (0.3333333333333333333333 - t * 0.25));
        let u = INV_LN2_H * t; // INV_LN2_H has 21 sig. bits
        let v = t * INV_LN2_L - w * INV_LN2;
        let t1 = with_lo(u + v, 0);
        (t1, v - (t1 - u))
    } else {
        const CP: f64 = 0.9617966939259756; // 0x1.ec709dc3a03fdp-1, 2/(3ln2)
        const CP_H: f64 = 0.9617967009544373; // 0x1.ec709ep-1, (float)cp
        const CP_L: f64 = -7.028461650952758e-09; // 0x1.e2fe0145b01f5p-28, tail of CP_H

        const BP: [f64; 2] = [1.0, 1.5];
        const DP_H: [f64; 2] = [0.0, 0.5849624872207642]; // 0x1.2b8034p-1
        const DP_L: [f64; 2] = [0.0, 1.350039202129749e-08]; // 0x1.cfdeb43cfd006p-27

        // Poly coefs for (3/2)*(log(x)-2s-2/3*s**3
        const L1: f64 = 0.5999999999999946; // 0x1.3333333333303p-1
        const L2: f64 = 0.4285714285785502; // 0x1.b6db6db6fabffp-2
        const L3: f64 = 0.33333332981837743; // 0x1.55555518f264dp-2
        const L4: f64 = 0.272728123808534; // 0x1.17460a91d4101p-2
        const L5: f64 = 0.23066074577556175; // 0x1.d864a93c9db65p-3
        const L6: f64 = 0.20697501780033842; // 0x1.a7e284a454eefp-3

        n = 0;
        // Take care of subnormal numbers
        if ix < 0x00100000 {
            x_abs *= 9007199254740992.0;
            n -= 53;
            ix = hi(x_abs);
        }
        n += (ix >> 20) - 0x3ff;
        let j = ix & 0x000fffff;
        // Determine interval
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
        x_abs = with_hi(x_abs, ix);

        // Compute ss = s_h + s_l = (x-1)/(x+1) or (x-1.5)/(x+1.5)
        let u = x_abs - BP[k];
        let v = 1.0 / (x_abs + BP[k]);
        let ss = u * v;
        let s_h = with_lo(ss, 0);
        // t_h = x_abs + BP[k] High
        let t_h = from_hi_lo(
            ((ix >> 1) | 0x20000000) + 0x00080000 + ((k as i32) << 18),
            0,
        );
        let t_l = x_abs - (t_h - BP[k]);
        let s_l = v * ((u - s_h * t_h) - s_h * t_l);
        // Compute log(x_abs)
        let mut s2 = ss * ss;
        let mut r = s2 * s2 * (L1 + s2 * (L2 + s2 * (L3 + s2 * (L4 + s2 * (L5 + s2 * L6)))));
        r += s_l * (s_h + ss);
        s2 = s_h * s_h;
        let t_h = with_lo(3.0 + s2 + r, 0);
        let t_l = r - ((t_h - 3.0) - s2);
        // u+v = ss*(1+...)
        let u = s_h * t_h;
        let v = s_l * t_h + t_l * ss;
        // 2/(3log2)*(ss + ...)
        let p_h = with_lo(u + v, 0);
        let p_l = v - (p_h - u);
        let z_h = CP_H * p_h; // CP_H + CP_L = 2/(3*log2)
        let z_l = CP_L * p_h + p_l * CP + DP_L[k];
        // log2(x_abs) = (ss + ..)*2/(3*log2) = n + DP_H + z_h + z_l
        let t = f64::from(n);
        let t1 = with_lo(((z_h + z_l) + DP_H[k]) + t, 0);
        (t1, z_l - (((t1 - t) - DP_H[k]) - z_h))
    };

    // Split up y into (y1 + y2) and compute (y1 + y2) * (t1 + t2)
    let y1 = with_lo(y, 0);
    let p_l = (y - y1) * t1 + y * t2;
    let mut p_h = y1 * t1;
    let mut z = p_l + p_h;
    let mut j = hi(z);
    let i = lo(z);
    if j >= 0x40900000 {
        // z >= 1024
        if ((j - 0x40900000) | i) != 0 {
            return s * INFINITY; // overflow
        } else {
            const OVT: f64 = 8.0085662595372944372e-0017; // -(1024-log2(ovfl+.5ulp))
            if p_l + OVT > z - p_h {
                return s * INFINITY; // overflow
            }
        }
    } else if (j & EXP_SIGNIF_BITS) >= 0x4090cc00 {
        // z <= -1075
        // z < -1075, or z == -1075 and p_l rounds it down
        if (j.wrapping_sub(0xc090cc00_u32 as i32) | i) != 0 || p_l <= z - p_h {
            return s * 0.0; // underflow
        }
    }

    // Compute 2**(p_h+p_l)
    const P1: f64 = 0.16666666666666602; // 0x1.555555555553ep-3
    const P2: f64 = -0.0027777777777015593; // 0x1.6c16c16bebd93p-9
    const P3: f64 = 6.613756321437934e-05; // 0x1.1566aaf25de2cp-14
    const P4: f64 = -1.6533902205465252e-06; // 0x1.bbd41c5d26bf1p-20
    const P5: f64 = 4.1381367970572385e-08; // 0x1.6376972bea4d0p-25
    const LG2: f64 = 0.6931471805599453; // 0x1.62e42fefa39efp-1
    const LG2_H: f64 = 0.6931471824645996; // 0x1.62e43p-1
    const LG2_L: f64 = -1.904654299957768e-09; // 0x1.05c610ca86c39p-29
    let i = j & EXP_SIGNIF_BITS;
    let mut k = (i >> 20) - 0x3ff;
    let mut n = 0;
    if i > 0x3fe00000 {
        // if |z| > 0.5, set n = [z + 0.5]
        n = j.wrapping_add(0x00100000 >> (k + 1));
        k = ((n & EXP_SIGNIF_BITS) >> 20) - 0x3ff; // new k for n
        let t = from_hi_lo(n & !(0x000fffff >> k), 0);
        n = ((n & 0x000fffff) | 0x00100000) >> (20 - k);
        if j < 0 {
            n = -n;
        }
        p_h -= t;
    }
    let t = with_lo(p_l + p_h, 0);
    let u = t * LG2_H;
    let v = (p_l - (t - p_h)) * LG2 + t * LG2_L;
    z = u + v;
    let w = v - (z - u);
    let t = z * z;
    let t1 = z - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    let r = (z * t1) / (t1 - 2.0) - (w + z * w);
    z = 1.0 - (r - z);
    j = hi(z);
    j = j.wrapping_add(n << 20);
    if (j >> 20) <= 0 {
        z = scalb(z, n); // subnormal output
    } else {
        z = with_hi(z, hi(z).wrapping_add(n << 20));
    }
    s * z
}

// ---------------------------------------------------------------------------
// exp / log / log10
// ---------------------------------------------------------------------------

/// `StrictMath.exp`.
///
/// Reduces x = k*ln2 + r with |r| <= 0.5*ln2, approximates exp(r) with a
/// degree-5 rational kernel, and scales back by 2^k.
pub(crate) fn exp(mut x: f64) -> f64 {
    const HALF: [f64; 2] = [0.5, -0.5];
    const TWOM1000: f64 = 9.332636185032189e-302; // 0x1.0p-1000
    const O_THRESHOLD: f64 = 709.782712893384; // 0x1.62e42fefa39efp9
    const U_THRESHOLD: f64 = -745.1332191019411; // 0x1.74910d52d3051p9
    const LN2HI: [f64; 2] = [0.6931471803691238, -0.6931471803691238];
    const LN2LO: [f64; 2] = [1.9082149292705877e-10, -1.9082149292705877e-10];
    const INVLN2: f64 = 1.4426950408889634; // 0x1.71547652b82fep0
    const P1: f64 = 0.16666666666666602; // 0x1.555555555553ep-3
    const P2: f64 = -0.0027777777777015593; // 0x1.6c16c16bebd93p-9
    const P3: f64 = 6.613756321437934e-05; // 0x1.1566aaf25de2cp-14
    const P4: f64 = -1.6533902205465252e-06; // 0x1.bbd41c5d26bf1p-20
    const P5: f64 = 4.1381367970572385e-08; // 0x1.6376972bea4d0p-25

    let mut hi_part = 0.0;
    let mut lo_part = 0.0;
    let mut k = 0;

    let mut hx = hi(x);
    let xsb = ((hx >> 31) & 1) as usize; // sign bit of x
    hx &= EXP_SIGNIF_BITS; // high word of |x|

    // filter out non-finite argument
    if hx >= 0x40862E42 {
        // |x| >= 709.78...
        if hx >= 0x7ff00000 {
            if ((hx & 0xfffff) | lo(x)) != 0 {
                return x + x; // NaN
            } else {
                return if xsb == 0 { x } else { 0.0 }; // exp(+-inf) = {inf, 0}
            }
        }
        if x > O_THRESHOLD {
            return HUGE * HUGE; // overflow
        }
        if x < U_THRESHOLD {
            return TWOM1000 * TWOM1000; // underflow
        }
    }

    // argument reduction
    if hx > 0x3fd62e42 {
        // |x| > 0.5 ln2
        if hx < 0x3FF0A2B2 {
            // and |x| < 1.5 ln2
            hi_part = x - LN2HI[xsb];
            lo_part = LN2LO[xsb];
            k = 1 - xsb as i32 - xsb as i32;
        } else {
            k = (INVLN2 * x + HALF[xsb]) as i32;
            let t = f64::from(k);
            hi_part = x - t * LN2HI[0]; // t*ln2HI is exact here
            lo_part = t * LN2LO[0];
        }
        x = hi_part - lo_part;
    } else if hx < 0x3e300000 {
        // |x| < 2**-28
        if HUGE + x > 1.0 {
            return 1.0 + x; // trigger inexact
        }
    }

    // x is now in primary range
    let t = x * x;
    let c = x - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    if k == 0 {
        return 1.0 - ((x * c) / (c - 2.0) - x);
    }
    let y = 1.0 - ((lo_part - (x * c) / (2.0 - c)) - hi_part);
    if k >= -1021 {
        with_hi(y, hi(y).wrapping_add(k << 20)) // add k to y's exponent
    } else {
        with_hi(y, hi(y).wrapping_add((k + 1000) << 20)) * TWOM1000
    }
}

/// `StrictMath.log` (natural logarithm).
///
/// Reduces x = 2^k * (1+f) with sqrt(2)/2 < 1+f < sqrt(2), then
/// log(1+f) = 2s + s*R(s^2) where s = f/(2+f).
pub(crate) fn log(mut x: f64) -> f64 {
    const LN2_HI: f64 = 0.6931471803691238; // 0x1.62e42feep-1
    const LN2_LO: f64 = 1.9082149292705877e-10; // 0x1.a39ef35793c76p-33
    const LG1: f64 = 0.6666666666666735; // 0x1.5555555555593p-1
    const LG2: f64 = 0.3999999999940942; // 0x1.999999997fa04p-2
    const LG3: f64 = 0.2857142874366239; // 0x1.2492494229359p-2
    const LG4: f64 = 0.22222198432149784; // 0x1.c71c51d8e78afp-3
    const LG5: f64 = 0.1818357216161805; // 0x1.7466496cb03dep-3
    const LG6: f64 = 0.15313837699209373; // 0x1.39a09d078c69fp-3
    const LG7: f64 = 0.14798198605116586; // 0x1.2f112df3e5244p-3

    let mut hx = hi(x);
    let lx = lo(x);

    let mut k = 0;
    if hx < 0x0010_0000 {
        // x < 2**-1022
        if ((hx & EXP_SIGNIF_BITS) | lx) == 0 {
            return -TWO54 / 0.0; // log(+-0) = -inf
        }
        if hx < 0 {
            return (x - x) / 0.0; // log(-#) = NaN
        }
        k -= 54;
        x *= TWO54; // subnormal number, scale up x
        hx = hi(x);
    }
    if hx >= EXP_BITS {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x9_5f64) & 0x10_0000;
    x = with_hi(x, hx | (i ^ 0x3ff0_0000)); // normalize x or x/2
    k += i >> 20;
    let f = x - 1.0;
    if (0x000f_ffff & (2 + hx)) < 3 {
        // |f| < 2**-20
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            let dk = f64::from(k);
            return dk * LN2_HI + dk * LN2_LO;
        }
        let r = f * f * (0.5 - 0.33333333333333333 * f);
        if k == 0 {
            return f - r;
        }
        let dk = f64::from(k);
        return dk * LN2_HI - ((r - dk * LN2_LO) - f);
    }
    let s = f / (2.0 + f);
    let dk = f64::from(k);
    let z = s * s;
    let mut i = hx - 0x6_147a;
    let w = z * z;
    let j = 0x6b851 - hx;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    i |= j;
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

/// `StrictMath.log10`.
///
/// log10(x) = n*log10_2hi + (n*log10_2lo + ivln10*log(x/2^n)).
pub(crate) fn log10(mut x: f64) -> f64 {
    const IVLN10: f64 = 0.4342944819032518; // 0x1.bcb7b1526e50ep-2
    const LOG10_2HI: f64 = 0.30102999566361177; // 0x1.34413509f6p-2
    const LOG10_2LO: f64 = 3.694239077158931e-13; // 0x1.9fef311f12b36p-42

    let mut hx = hi(x);
    let lx = lo(x);

    let mut k = 0;
    if hx < 0x0010_0000 {
        // x < 2**-1022
        if ((hx & EXP_SIGNIF_BITS) | lx) == 0 {
            return -TWO54 / 0.0; // log(+-0) = -inf
        }
        if hx < 0 {
            return (x - x) / 0.0; // log(-#) = NaN
        }
        k -= 54;
        x *= TWO54; // subnormal number, scale up x
        hx = hi(x);
    }
    if hx >= EXP_BITS {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    let i = ((k as u32) >> 31) as i32; // unsigned shift
    hx = (hx & 0x000f_ffff) | ((0x3ff - i) << 20);
    let y = f64::from(k + i);
    x = with_hi(x, hx);
    let z = y * LOG10_2LO + IVLN10 * log(x);
    z + y * LOG10_2HI
}

#[cfg(test)]
mod tests {
    //! Regression vectors produced by `java.lang.StrictMath` on JDK 21, as
    //! raw IEEE 754 bit patterns: `(x, expected)` or `(x, y, expected)`.
    //! Any NaN result is accepted for an expected NaN.

    use super::*;

    fn check1(name: &str, f: fn(f64) -> f64, cases: &[(u64, u64)]) {
        for &(x, want) in cases {
            let x = f64::from_bits(x);
            let got = f(x);
            let want = f64::from_bits(want);
            assert!(
                (got.is_nan() && want.is_nan()) || got.to_bits() == want.to_bits(),
                "{name}({x:e}): got {got:e} ({:#018x}), want {want:e} ({:#018x})",
                got.to_bits(),
                want.to_bits(),
            );
        }
    }

    fn check2(name: &str, f: fn(f64, f64) -> f64, cases: &[(u64, u64, u64)]) {
        for &(x, y, want) in cases {
            let (x, y) = (f64::from_bits(x), f64::from_bits(y));
            let got = f(x, y);
            let want = f64::from_bits(want);
            assert!(
                (got.is_nan() && want.is_nan()) || got.to_bits() == want.to_bits(),
                "{name}({x:e}, {y:e}): got {got:e} ({:#018x}), want {want:e} ({:#018x})",
                got.to_bits(),
                want.to_bits(),
            );
        }
    }

    const SIN: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x0000_0000_0000_0000),
        (0x8000_0000_0000_0000, 0x8000_0000_0000_0000),
        (0x0000_0000_0000_0001, 0x0000_0000_0000_0001),
        (0x0010_0000_0000_0000, 0x0010_0000_0000_0000),
        (0x7ff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x3fea_ed54_8f09_0cee),
        (0xbff0_0000_0000_0000, 0xbfea_ed54_8f09_0cee),
        (0x3fe0_0000_0000_0000, 0x3fde_aee8_744b_05f0),
        (0x3fb9_9999_9999_999a, 0x3fb9_8eae_cb8b_cb2c),
        (0x01a5_6e1f_c2f8_f359, 0x01a5_6e1f_c2f8_f359),
        (0x7e37_e43c_8800_759c, 0xbfea_2c16_b010_e385),
        (0xfe37_e43c_8800_759c, 0x3fea_2c16_b010_e385),
        (0x4480_f0cf_064d_d592, 0xbfeb_453a_b76b_f397),
        (0x4009_21fb_5444_2d18, 0x3ca1_a626_3314_5c07),
        (0x3ff9_21fb_5444_2d18, 0x3ff0_0000_0000_0000),
        (0x4086_2e42_fefa_39ef, 0xbfcb_963d_50b6_322a),
        (0xc087_4910_d52d_3051, 0x3fe1_6c51_c71d_9462),
        (0x412e_8480_0000_0000, 0xbfd6_664b_2568_d867),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0x4090_c7ff_ffff_ffff, 0xbfda_5ecc_9be3_3231),
        (0x084a_5b9e_728a_70ab, 0x084a_5b9e_728a_70ab),
        (0x85ae_7484_0d19_68af, 0x85ae_7484_0d19_68af),
        (0xf36e_3678_2bc5_3b2f, 0x3fef_9057_980e_4f0a),
        (0xa6c7_c6a1_bc0e_1d9e, 0xa6c7_c6a1_bc0e_1d9e),
        (0x0073_ba94_e9f0_7cfe, 0x0073_ba94_e9f0_7cfe),
        (0xc1ad_d6ec_fab7_aa66, 0x3fe6_8bc1_b33b_f9f6),
        (0x3ea5_cbc2_3079_1df8, 0x3ea5_cbc2_3079_1c49),
        (0x3d08_b9af_ecf8_cc56, 0x3d08_b9af_ecf8_cc56),
        (0x4007_5a62_2fc1_dbc8, 0x3fcc_3d98_54cd_0994),
        (0xbff7_4e1a_6bd4_a540, 0xbfef_ca9c_f978_53fe),
        (0xbf9a_f265_9f98_7000, 0xbf9a_f199_ceaf_cd84),
        (0x0008_febb_bb8e_637e, 0x0008_febb_bb8e_637e),
        (0xc063_3e04_6c84_3285, 0xbd29_60cd_4fc6_e59d),
        (0x403d_d85a_7410_f58d, 0xbff0_0000_0000_0000),
        (0x406a_b41b_0988_6feb, 0x3d23_4fdd_da6e_978e),
        (0x4139_0ced_ed4d_c73d, 0x3ff0_0000_0000_0000),
        (0x4103_711e_0405_0724, 0x3ff0_0000_0000_0000),
        (0xea9f_a050_3a51_3a84, 0xbfee_d20a_e1eb_3777),
        (0xdba5_11bb_afd8_6a88, 0x3fef_5cde_cfb7_306c),
        (0x54e2_814c_b9af_3b10, 0xbfeb_94f9_9b83_4078),
        (0x4128_f11f_06c4_bcb4, 0xbfe6_0954_a22d_0ea2),
        (0x412e_3911_8d7c_0194, 0x3fef_d3e9_c3ab_2934),
        (0x4002_d97c_7f33_21d2, 0x3fe6_a09e_667f_3bcd),
    ];
    const COS: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x3ff0_0000_0000_0000),
        (0x8000_0000_0000_0000, 0x3ff0_0000_0000_0000),
        (0x0000_0000_0000_0001, 0x3ff0_0000_0000_0000),
        (0x0010_0000_0000_0000, 0x3ff0_0000_0000_0000),
        (0x7ff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x3fe1_4a28_0fb5_068c),
        (0xbff0_0000_0000_0000, 0x3fe1_4a28_0fb5_068c),
        (0x3fe0_0000_0000_0000, 0x3fec_1528_065b_7d50),
        (0x3fb9_9999_9999_999a, 0x3fef_d712_f9a8_17c0),
        (0x01a5_6e1f_c2f8_f359, 0x3ff0_0000_0000_0000),
        (0x7e37_e43c_8800_759c, 0xbfe2_6990_22ad_c4c1),
        (0xfe37_e43c_8800_759c, 0xbfe2_6990_22ad_c4c1),
        (0x4480_f0cf_064d_d592, 0x3fe0_be2c_ef01_c8f4),
        (0x4009_21fb_5444_2d18, 0xbff0_0000_0000_0000),
        (0x3ff9_21fb_5444_2d18, 0x3c91_a626_3314_5c07),
        (0x4086_2e42_fefa_39ef, 0x3fef_3f7a_97bc_6780),
        (0xc087_4910_d52d_3051, 0xbfea_d746_4b59_047f),
        (0x412e_8480_0000_0000, 0x3fed_f9df_9906_d32c),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0x4090_c7ff_ffff_ffff, 0x3fed_2848_cd5d_0a27),
        (0x084a_5b9e_728a_70ab, 0x3ff0_0000_0000_0000),
        (0x85ae_7484_0d19_68af, 0x3ff0_0000_0000_0000),
        (0xf36e_3678_2bc5_3b2f, 0xbfc5_0fbf_9fb5_f311),
        (0xa6c7_c6a1_bc0e_1d9e, 0x3ff0_0000_0000_0000),
        (0x0073_ba94_e9f0_7cfe, 0x3ff0_0000_0000_0000),
        (0xc1ad_d6ec_fab7_aa66, 0xbfe6_b567_ef5c_143a),
        (0x3ea5_cbc2_3079_1df8, 0x3fef_ffff_ffff_f894),
        (0x3d08_b9af_ecf8_cc56, 0x3ff0_0000_0000_0000),
        (0x4007_5a62_2fc1_dbc8, 0xbfef_3621_3883_859a),
        (0xbff7_4e1a_6bd4_a540, 0x3fbd_2dc9_9fe1_ecce),
        (0xbf9a_f265_9f98_7000, 0x3fef_fd29_e891_6d15),
        (0x0008_febb_bb8e_637e, 0x3ff0_0000_0000_0000),
        (0xc063_3e04_6c84_3285, 0xbff0_0000_0000_0000),
        (0x403d_d85a_7410_f58d, 0x3cc6_1565_46af_a570),
        (0x406a_b41b_0988_6feb, 0x3ff0_0000_0000_0000),
        (0x4139_0ced_ed4d_c73d, 0xbdcf_b684_d316_1aae),
        (0x4103_711e_0405_0724, 0x3da1_7a17_8fc6_715a),
        (0xea9f_a050_3a51_3a84, 0xbfd1_374f_1bc8_b6c3),
        (0xdba5_11bb_afd8_6a88, 0x3fc9_6abd_a928_ff6d),
        (0x54e2_814c_b9af_3b10, 0xbfe0_397d_f279_7fa9),
        (0x4128_f11f_06c4_bcb4, 0xbfe7_340e_129e_cd89),
        (0x412e_3911_8d7c_0194, 0xbfba_85fe_a2c5_8261),
        (0x4002_d97c_7f33_21d2, 0xbfe6_a09e_667f_3bcc),
    ];
    const TAN: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x0000_0000_0000_0000),
        (0x8000_0000_0000_0000, 0x8000_0000_0000_0000),
        (0x0000_0000_0000_0001, 0x0000_0000_0000_0001),
        (0x0010_0000_0000_0000, 0x0010_0000_0000_0000),
        (0x7ff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x3ff8_eb24_5cbe_e3a6),
        (0xbff0_0000_0000_0000, 0xbff8_eb24_5cbe_e3a6),
        (0x3fe0_0000_0000_0000, 0x3fe1_7b4f_5bf3_474a),
        (0x3fb9_9999_9999_999a, 0x3fb9_af88_7743_0b80),
        (0x01a5_6e1f_c2f8_f359, 0x01a5_6e1f_c2f8_f359),
        (0x7e37_e43c_8800_759c, 0x3ff6_be41_1f37_ac77),
        (0xfe37_e43c_8800_759c, 0xbff6_be41_1f37_ac77),
        (0x4480_f0cf_064d_d592, 0xbffa_0f79_c1b6_b258),
        (0x4009_21fb_5444_2d18, 0xbca1_a626_3314_5c07),
        (0x3ff9_21fb_5444_2d18, 0x434d_0296_7c31_cdb5),
        (0x4086_2e42_fefa_39ef, 0xbfcc_4034_5185_1ab4),
        (0xc087_4910_d52d_3051, 0xbfe4_c5a2_cec8_c14a),
        (0x412e_8480_0000_0000, 0xbfd7_e976_8ab7_34c0),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0x4090_c7ff_ffff_ffff, 0xbfdc_f0f4_7e38_e065),
        (0x084a_5b9e_728a_70ab, 0x084a_5b9e_728a_70ab),
        (0x85ae_7484_0d19_68af, 0x85ae_7484_0d19_68af),
        (0xf36e_3678_2bc5_3b2f, 0xc017_fa78_13ee_1488),
        (0xa6c7_c6a1_bc0e_1d9e, 0xa6c7_c6a1_bc0e_1d9e),
        (0x0073_ba94_e9f0_7cfe, 0x0073_ba94_e9f0_7cfe),
        (0xc1ad_d6ec_fab7_aa66, 0xbfef_c54f_3d85_9e84),
        (0x3ea5_cbc2_3079_1df8, 0x3ea5_cbc2_3079_2157),
        (0x3d08_b9af_ecf8_cc56, 0x3d08_b9af_ecf8_cc56),
        (0x4007_5a62_2fc1_dbc8, 0xbfcc_f440_1a6f_5c51),
        (0xbff7_4e1a_6bd4_a540, 0xc021_6ebe_ccea_2564),
        (0xbf9a_f265_9f98_7000, 0xbf9a_f3fd_61f0_ec23),
        (0x0008_febb_bb8e_637e, 0x0008_febb_bb8e_637e),
        (0xc063_3e04_6c84_3285, 0x3d29_60cd_4fc6_e59d),
        (0x403d_d85a_7410_f58d, 0xc317_2f45_3d4f_5dec),
        (0x406a_b41b_0988_6feb, 0x3d23_4fdd_da6e_978e),
        (0x4139_0ced_ed4d_c73d, 0xc210_2512_b7dc_5ccb),
        (0x4103_711e_0405_0724, 0x423d_4bb8_00d3_dc7e),
        (0xea9f_a050_3a51_3a84, 0x400c_a4bb_4a5b_d13e),
        (0xdba5_11bb_afd8_6a88, 0x4013_be2e_9bf3_3d59),
        (0x54e2_814c_b9af_3b10, 0x3ffb_333c_f5d5_b017),
        (0x4128_f11f_06c4_bcb4, 0x3fee_6406_938a_38a0),
        (0x412e_3911_8d7c_0194, 0xc023_3326_c628_521e),
        (0x4002_d97c_7f33_21d2, 0xbff0_0000_0000_0001),
    ];
    const ASIN: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x0000_0000_0000_0000),
        (0x8000_0000_0000_0000, 0x8000_0000_0000_0000),
        (0x0000_0000_0000_0001, 0x0000_0000_0000_0001),
        (0x0010_0000_0000_0000, 0x0010_0000_0000_0000),
        (0x7ff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x3ff9_21fb_5444_2d18),
        (0xbff0_0000_0000_0000, 0xbff9_21fb_5444_2d18),
        (0x3fe0_0000_0000_0000, 0x3fe0_c152_382d_7366),
        (0x3fb9_9999_9999_999a, 0x3fb9_a492_7603_7884),
        (0x01a5_6e1f_c2f8_f359, 0x01a5_6e1f_c2f8_f359),
        (0x7e37_e43c_8800_759c, 0xfff8_0000_0000_0000),
        (0xfe37_e43c_8800_759c, 0xfff8_0000_0000_0000),
        (0x4480_f0cf_064d_d592, 0xfff8_0000_0000_0000),
        (0x4009_21fb_5444_2d18, 0xfff8_0000_0000_0000),
        (0x3ff9_21fb_5444_2d18, 0xfff8_0000_0000_0000),
        (0x4086_2e42_fefa_39ef, 0xfff8_0000_0000_0000),
        (0xc087_4910_d52d_3051, 0xfff8_0000_0000_0000),
        (0x412e_8480_0000_0000, 0xfff8_0000_0000_0000),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0x3fdc_0000_0000_0001, 0x3fdc_faf2_7460_fea0),
        (0x092b_b148_b367_e168, 0x092b_b148_b367_e168),
        (0x1491_43a0_8777_d3e1, 0x1491_43a0_8777_d3e1),
        (0x8dc4_6690_29e8_92a9, 0x8dc4_6690_29e8_92a9),
        (0xfe8b_ba65_c148_0a31, 0xfff8_0000_0000_0000),
        (0xfa52_d8d1_39fd_64ec, 0xfff8_0000_0000_0000),
        (0x0b41_d9a0_0ec6_cdfe, 0x0b41_d9a0_0ec6_cdfe),
        (0xc070_b256_9a23_2310, 0xfff8_0000_0000_0000),
        (0xc177_36eb_8e68_4b1a, 0xfff8_0000_0000_0000),
        (0x3d0d_7c67_cb32_be66, 0x3d0d_7c67_cb32_be66),
        (0x4013_4b86_7b49_0e8d, 0xfff8_0000_0000_0000),
        (0x3fb3_cebd_d697_4d50, 0x3fb3_d3d0_8f2b_b23a),
        (0xbfe0_748c_77fc_b6f4, 0xbfe1_488c_f16f_6d68),
        (0xbfee_248d_6bf1_f91c, 0xbff3_a7a3_56cd_428e),
        (0x000b_1033_b0e2_2add, 0x000b_1033_b0e2_2add),
        (0x3fe8_fda5_9ef1_3449, 0x3fec_adb7_c835_8eca),
        (0x3fed_9e46_ebb4_9653, 0x3ff2_ebb0_07bb_903e),
        (0x3fe3_a5d3_303b_4c5a, 0x3fe5_27cd_56ae_4b3e),
        (0xbfe7_f1eb_8d99_d700, 0xbfeb_0e11_ac0a_1841),
        (0xbfed_c784_ec94_813a, 0xbff3_2313_468e_0045),
        (0xbfe5_4619_1fe3_d78b, 0xbfe7_4582_f480_f702),
        (0xbfed_2a17_af88_af48, 0xbff2_58a8_3740_04e3),
        (0x3fef_ffff_ffff_fff9, 0x3ff9_21fb_49ae_ed43),
        (0x3fef_ffff_ffff_ff02, 0x3ff9_21fb_1484_4d38),
    ];
    const ACOS: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x3ff9_21fb_5444_2d18),
        (0x8000_0000_0000_0000, 0x3ff9_21fb_5444_2d18),
        (0x0000_0000_0000_0001, 0x3ff9_21fb_5444_2d18),
        (0x0010_0000_0000_0000, 0x3ff9_21fb_5444_2d18),
        (0x7ff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x0000_0000_0000_0000),
        (0xbff0_0000_0000_0000, 0x4009_21fb_5444_2d18),
        (0x3fe0_0000_0000_0000, 0x3ff0_c152_382d_7366),
        (0x3fb9_9999_9999_999a, 0x3ff7_87b2_2ce3_f590),
        (0x01a5_6e1f_c2f8_f359, 0x3ff9_21fb_5444_2d18),
        (0x7e37_e43c_8800_759c, 0xfff8_0000_0000_0000),
        (0xfe37_e43c_8800_759c, 0xfff8_0000_0000_0000),
        (0x4480_f0cf_064d_d592, 0xfff8_0000_0000_0000),
        (0x4009_21fb_5444_2d18, 0xfff8_0000_0000_0000),
        (0x3ff9_21fb_5444_2d18, 0xfff8_0000_0000_0000),
        (0x4086_2e42_fefa_39ef, 0xfff8_0000_0000_0000),
        (0xc087_4910_d52d_3051, 0xfff8_0000_0000_0000),
        (0x412e_8480_0000_0000, 0xfff8_0000_0000_0000),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0x3fdc_0000_0000_0001, 0x3ff1_e33e_b72b_ed70),
        (0x092b_b148_b367_e168, 0x3ff9_21fb_5444_2d18),
        (0x1491_43a0_8777_d3e1, 0x3ff9_21fb_5444_2d18),
        (0x8dc4_6690_29e8_92a9, 0x3ff9_21fb_5444_2d18),
        (0xfe8b_ba65_c148_0a31, 0xfff8_0000_0000_0000),
        (0xfa52_d8d1_39fd_64ec, 0xfff8_0000_0000_0000),
        (0x0b41_d9a0_0ec6_cdfe, 0x3ff9_21fb_5444_2d18),
        (0xc070_b256_9a23_2310, 0xfff8_0000_0000_0000),
        (0xc177_36eb_8e68_4b1a, 0xfff8_0000_0000_0000),
        (0x3d0d_7c67_cb32_be66, 0x3ff9_21fb_5444_2cdd),
        (0x4013_4b86_7b49_0e8d, 0xfff8_0000_0000_0000),
        (0x3fb3_cebd_d697_4d50, 0x3ff7_e4be_4b51_71f5),
        (0xbfe0_748c_77fc_b6f4, 0x4000_e320_e67d_f1e6),
        (0xbfee_248d_6bf1_f91c, 0x4006_64cf_5588_b7d3),
        (0x000b_1033_b0e2_2add, 0x3ff9_21fb_5444_2d18),
        (0x3fe8_fda5_9ef1_3449, 0x3fe5_963e_e052_cb67),
        (0x3fed_9e46_ebb4_9653, 0x3fd8_d92d_3222_7368),
        (0x3fe3_a5d3_303b_4c5a, 0x3fed_1c29_51da_0ef2),
        (0xbfe7_f1eb_8d99_d700, 0x4003_5482_1524_9c9c),
        (0xbfed_c784_ec94_813a, 0x4006_2287_4d69_16af),
        (0xbfe5_4619_1fe3_d78b, 0x4002_625e_6742_544d),
        (0xbfed_2a17_af88_af48, 0x4005_bd51_c5c2_18fe),
        (0x3fef_ffff_ffff_fff9, 0x3e65_2a7f_a9d2_f8ea),
        (0x3fef_ffff_ffff_ff02, 0x3e8f_dfef_efeb_e3eb),
    ];
    const ATAN: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x0000_0000_0000_0000),
        (0x8000_0000_0000_0000, 0x8000_0000_0000_0000),
        (0x0000_0000_0000_0001, 0x0000_0000_0000_0001),
        (0x0010_0000_0000_0000, 0x0010_0000_0000_0000),
        (0x7ff0_0000_0000_0000, 0x3ff9_21fb_5444_2d18),
        (0xfff0_0000_0000_0000, 0xbff9_21fb_5444_2d18),
        (0x3ff0_0000_0000_0000, 0x3fe9_21fb_5444_2d18),
        (0xbff0_0000_0000_0000, 0xbfe9_21fb_5444_2d18),
        (0x3fe0_0000_0000_0000, 0x3fdd_ac67_0561_bb4f),
        (0x3fb9_9999_9999_999a, 0x3fb9_83e2_82e2_cc4d),
        (0x01a5_6e1f_c2f8_f359, 0x01a5_6e1f_c2f8_f359),
        (0x7e37_e43c_8800_759c, 0x3ff9_21fb_5444_2d18),
        (0xfe37_e43c_8800_759c, 0xbff9_21fb_5444_2d18),
        (0x4480_f0cf_064d_d592, 0x3ff9_21fb_5444_2d18),
        (0x4009_21fb_5444_2d18, 0x3ff4_33b8_a322_ddd2),
        (0x3ff9_21fb_5444_2d18, 0x3ff0_0fe9_87ed_02ff),
        (0x4086_2e42_fefa_39ef, 0x3ff9_1c36_02aa_f162),
        (0xc087_4910_d52d_3051, 0xbff9_1c7c_18da_8562),
        (0x412e_8480_0000_0000, 0x3ff9_21fa_47d4_b30d),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0xbe3f_ffff_ffff_ffff, 0xbe3f_ffff_ffff_ffff),
        (0x8ca3_e092_bb2b_1fd1, 0x8ca3_e092_bb2b_1fd1),
        (0x132d_80bb_3dd9_c5b6, 0x132d_80bb_3dd9_c5b6),
        (0xeefc_7569_8b2f_3ac5, 0xbff9_21fb_5444_2d18),
        (0xb866_371a_56ee_c6e6, 0xb866_371a_56ee_c6e6),
        (0xbe82_7111_f1bc_366e, 0xbe82_7111_f1bc_364d),
        (0x4460_5d15_0e36_e76a, 0x3ff9_21fb_5444_2d18),
        (0x972f_4aba_8860_12bc, 0x972f_4aba_8860_12bc),
        (0x5960_1110_1e55_ded9, 0x3ff9_21fb_5444_2d18),
        (0xc83b_0649_2b03_b426, 0xbff9_21fb_5444_2d18),
        (0x701c_100c_6961_f273, 0x3ff9_21fb_5444_2d18),
        (0x5b58_9145_fc41_64db, 0x3ff9_21fb_5444_2d18),
        (0xbd3a_5880_8ea3_f9d4, 0xbd3a_5880_8ea3_f9d4),
        (0x4130_734f_7a8e_bd72, 0x3ff9_21fa_5b46_a053),
        (0xbd0a_7273_8813_87ac, 0xbd0a_7273_8813_87ac),
        (0x4155_b448_21d4_5541, 0x3ff9_21fb_2516_28d1),
        (0x3dc1_6eaa_8ba3_76eb, 0x3dc1_6eaa_8ba3_76eb),
        (0x3fe2_52b2_86c6_9210, 0x3fe0_a40b_4259_c79c),
        (0xbfe2_48c8_8ed0_35c0, 0xbfe0_9c92_f247_6661),
        (0xbfd7_ba3d_a0cd_c760, 0xbfd6_b8da_ba80_0b94),
        (0x3fe3_e03c_8b0b_ddb0, 0x3fe1_c92a_23a4_b584),
        (0xbfe8_4595_857d_af20, 0xbfe4_c3e9_e2dd_905d),
        (0x0009_8646_2fac_f56e, 0x0009_8646_2fac_f56e),
        (0x0006_4cb5_55c6_98a9, 0x0006_4cb5_55c6_98a9),
    ];
    const EXP: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x3ff0_0000_0000_0000),
        (0x8000_0000_0000_0000, 0x3ff0_0000_0000_0000),
        (0x0000_0000_0000_0001, 0x3ff0_0000_0000_0000),
        (0x0010_0000_0000_0000, 0x3ff0_0000_0000_0000),
        (0x7ff0_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0x0000_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x4005_bf0a_8b14_576a),
        (0xbff0_0000_0000_0000, 0x3fd7_8b56_362c_ef38),
        (0x3fe0_0000_0000_0000, 0x3ffa_6129_8e1e_069c),
        (0x3fb9_9999_9999_999a, 0x3ff1_aec7_b35a_00d4),
        (0x01a5_6e1f_c2f8_f359, 0x3ff0_0000_0000_0000),
        (0x7e37_e43c_8800_759c, 0x7ff0_0000_0000_0000),
        (0xfe37_e43c_8800_759c, 0x0000_0000_0000_0000),
        (0x4480_f0cf_064d_d592, 0x7ff0_0000_0000_0000),
        (0x4009_21fb_5444_2d18, 0x4037_2404_6eb0_9339),
        (0x3ff9_21fb_5444_2d18, 0x4013_3ded_c855_935f),
        (0x4086_2e42_fefa_39ef, 0x7fef_ffff_ffff_ff2a),
        (0xc087_4910_d52d_3051, 0x0000_0000_0000_0001),
        (0x412e_8480_0000_0000, 0x7ff0_0000_0000_0000),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0x3fdc_0000_0000_0001, 0x3ff8_c802_477b_0010),
        (0xf11f_ee81_9fb7_4a8e, 0x0000_0000_0000_0000),
        (0xc2ae_3648_1aa7_1c2f, 0x0000_0000_0000_0000),
        (0xf079_6356_3a83_3c3d, 0x0000_0000_0000_0000),
        (0xd55e_7d1a_12d0_9a24, 0x0000_0000_0000_0000),
        (0xba88_84e4_9a64_19fc, 0x3ff0_0000_0000_0000),
        (0xb554_8067_b0b0_8860, 0x3ff0_0000_0000_0000),
        (0x3c98_4244_a574_32aa, 0x3ff0_0000_0000_0000),
        (0x4149_acf3_057c_5485, 0x7ff0_0000_0000_0000),
        (0x3c93_df60_9d95_1fec, 0x3ff0_0000_0000_0000),
        (0x4044_7191_7d2d_c52c, 0x439f_bb38_dbf2_4f4a),
        (0xc07c_b5cd_aa02_a656, 0x1683_6c4a_6a55_f20c),
        (0xc07e_7c9b_9315_1f22, 0x13f3_4d60_6259_cd3e),
        (0xc065_02b5_1536_c828, 0x30c6_b56e_52c6_3efe),
        (0x0002_acd3_a3c2_1196, 0x3ff0_0000_0000_0000),
        (0xc01b_dccf_23c8_bd68, 0x3f4e_ecde_c411_95a2),
        (0xbfe0_027e_b45e_2c40, 0x3fe3_672f_a6b2_b472),
        (0xc032_7bf2_ee76_3f04, 0x3e44_2757_b60c_d992),
        (0xc086_f9f6_4a5f_358e, 0x0000_0000_0000_2676),
        (0xc086_8583_9a8a_e0f4, 0x0000_0004_cea0_cbea),
        (0xc087_3a89_1c0e_7708, 0x0000_0000_0000_0003),
        (0xc086_6e25_cc79_a1a0, 0x0000_0059_33c3_8c57),
        (0x4086_03cd_5909_0e9c, 0x7f74_4b3d_90ee_3d7e),
        (0x4085_f8a9_294d_1e41, 0x7f54_2a3a_2757_8b3f),
    ];
    const LOG: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0xfff0_0000_0000_0000),
        (0x8000_0000_0000_0000, 0xfff0_0000_0000_0000),
        (0x0000_0000_0000_0001, 0xc087_4385_446d_71c3),
        (0x0010_0000_0000_0000, 0xc086_232b_dd7a_bcd2),
        (0x7ff0_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x0000_0000_0000_0000),
        (0xbff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3fe0_0000_0000_0000, 0xbfe6_2e42_fefa_39ef),
        (0x3fb9_9999_9999_999a, 0xc002_6bb1_bbb5_5515),
        (0x01a5_6e1f_c2f8_f359, 0xc085_9634_47f8_7fb5),
        (0x7e37_e43c_8800_759c, 0x4085_9634_47f8_7fb5),
        (0xfe37_e43c_8800_759c, 0xfff8_0000_0000_0000),
        (0x4480_f0cf_064d_d592, 0x4049_5414_6219_54fe),
        (0x4009_21fb_5444_2d18, 0x3ff2_50d0_48e7_a1bd),
        (0x3ff9_21fb_5444_2d18, 0x3fdc_e6bb_25aa_1315),
        (0x4086_2e42_fefa_39ef, 0x401a_4284_94fa_f1b1),
        (0xc087_4910_d52d_3051, 0xfff8_0000_0000_0000),
        (0x412e_8480_0000_0000, 0x402b_a18a_998f_ffa0),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0xbfe6_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x917a_6d81_5a9b_759c, 0xfff8_0000_0000_0000),
        (0x5070_5294_5ba7_8f2c, 0x4066_e058_9b18_0062),
        (0xcf7f_a1af_8f66_89fb, 0xfff8_0000_0000_0000),
        (0xd0de_7743_dbab_17cd, 0xfff8_0000_0000_0000),
        (0x7bc7_3395_619f_a857, 0x4084_bdb5_34f6_7807),
        (0x6a91_709c_af71_2a45, 0x407d_8d00_32f0_205c),
        (0x3d74_9c1c_0717_7108, 0xc03b_7903_7715_77f6),
        (0xbea1_cb0a_1035_c902, 0xfff8_0000_0000_0000),
        (0xc151_b619_fbec_c680, 0xfff8_0000_0000_0000),
        (0x4092_1a03_0cfa_5d85, 0x401c_3833_7d38_0187),
        (0x4056_fde2_9219_3428, 0x4012_15f1_90f3_0cf4),
        (0x4052_6f9b_f478_c56b, 0x4011_33d0_0362_dee6),
        (0x404c_d545_1b92_e542, 0x4010_37fb_99da_1a94),
        (0x000e_1cf7_e697_6e37, 0xc086_242c_d9c3_4447),
        (0x63e4_eac4_a9cf_9927, 0x4078_ed3d_df1c_782e),
        (0x786f_c585_3f64_9d11, 0x4083_94c8_78fc_893e),
        (0x16d3_293e_a128_2feb, 0xc07c_7e91_9da3_baca),
        (0x3ff0_0000_0000_6e39, 0x3d9b_8e3f_ffff_a116),
        (0x3ff0_0000_2327_40a0, 0x3e81_93a0_3cb0_f95a),
        (0x3fef_a509_d32b_ed9a, 0xbf86_de1a_fc8d_81e2),
        (0x009c_16c5_c525_3575, 0xc085_f24e_c0a3_0a5f),
        (0x3f1a_36e2_eb1c_432d, 0xc022_6bb1_bbb5_5515),
        (0x7d98_7706_b021_3d0a, 0x4085_5ef1_32c5_5fb6),
    ];
    const LOG10: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0xfff0_0000_0000_0000),
        (0x8000_0000_0000_0000, 0xfff0_0000_0000_0000),
        (0x0000_0000_0000_0001, 0xc074_34e6_420f_4374),
        (0x0010_0000_0000_0000, 0xc073_3a71_46f7_2a42),
        (0x7ff0_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x0000_0000_0000_0000),
        (0xbff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x3fe0_0000_0000_0000, 0xbfd3_4413_509f_79ff),
        (0x3fb9_9999_9999_999a, 0xbff0_0000_0000_0000),
        (0x01a5_6e1f_c2f8_f359, 0xc072_c000_0000_0000),
        (0x7e37_e43c_8800_759c, 0x4072_c000_0000_0000),
        (0xfe37_e43c_8800_759c, 0xfff8_0000_0000_0000),
        (0x4480_f0cf_064d_d592, 0x4036_0000_0000_0000),
        (0x4009_21fb_5444_2d18, 0x3fdf_d14d_b31b_a3ba),
        (0x3ff9_21fb_5444_2d18, 0x3fc9_1a74_c4f8_5377),
        (0x4086_2e42_fefa_39ef, 0x4006_cf1a_d7ce_0276),
        (0xc087_4910_d52d_3051, 0xfff8_0000_0000_0000),
        (0x412e_8480_0000_0000, 0x4018_0000_0000_0000),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0xbfe6_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0x917a_6d81_5a9b_759c, 0xfff8_0000_0000_0000),
        (0x5070_5294_5ba7_8f2c, 0x4053_dec1_f267_bd08),
        (0xcf7f_a1af_8f66_89fb, 0xfff8_0000_0000_0000),
        (0xd0de_7743_dbab_17cd, 0xfff8_0000_0000_0000),
        (0x7bc7_3395_619f_a857, 0x4072_03f4_211e_290c),
        (0x6a91_709c_af71_2a45, 0x4069_aae0_4dbe_f57e),
        (0x3d74_9c1c_0717_7108, 0xc027_dccc_45c3_adc3),
        (0xbea1_cb0a_1035_c902, 0xfff8_0000_0000_0000),
        (0xc151_b619_fbec_c680, 0xfff8_0000_0000_0000),
        (0x4092_1a03_0cfa_5d85, 0x4008_82dc_82f6_b469),
        (0x4056_fde2_9219_3428, 0x3fff_6b09_2e12_fff3),
        (0x4052_6f9b_f478_c56b, 0x3ffd_e234_b9ee_baf0),
        (0x404c_d545_1b92_e542, 0x3ffc_2cbb_9f9f_2226),
        (0x000e_1cf7_e697_6e37, 0xc073_3b50_7df1_3c3d),
        (0x63e4_eac4_a9cf_9927, 0x4065_a6ad_151f_3058),
        (0x786f_c585_3f64_9d11, 0x4071_020c_4ea8_9745),
        (0x16d3_293e_a128_2feb, 0xc068_bffb_6217_9403),
        (0x3ff0_0000_0000_6e39, 0x3d87_ef3e_62fc_85b9),
        (0x3ff0_0000_2327_40a0, 0x3e6e_88a6_9d88_f941),
        (0x3fef_a509_d32b_ed9a, 0xbf73_dccf_b51e_cf2c),
        (0x009c_16c5_c525_3575, 0xc073_1000_0000_0000),
        (0x3f1a_36e2_eb1c_432d, 0xc010_0000_0000_0000),
        (0x7d98_7706_b021_3d0a, 0x4072_9000_0000_0000),
    ];
    const CBRT: &[(u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x0000_0000_0000_0000),
        (0x8000_0000_0000_0000, 0x8000_0000_0000_0000),
        (0x0000_0000_0000_0001, 0x2990_0000_0000_0000),
        (0x0010_0000_0000_0000, 0x2aa4_28a2_f98d_728b),
        (0x7ff0_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0xfff0_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x3ff0_0000_0000_0000),
        (0xbff0_0000_0000_0000, 0xbff0_0000_0000_0000),
        (0x3fe0_0000_0000_0000, 0x3fe9_65fe_a53d_6e3d),
        (0x3fb9_9999_9999_999a, 0x3fdd_b4c7_760b_cff3),
        (0x01a5_6e1f_c2f8_f359, 0x2b2b_ff2e_e48e_0530),
        (0x7e37_e43c_8800_759c, 0x54b2_49ad_2594_c37d),
        (0xfe37_e43c_8800_759c, 0xd4b2_49ad_2594_c37d),
        (0x4480_f0cf_064d_d592, 0x4174_8bd9_ae67_b4ba),
        (0x4009_21fb_5444_2d18, 0x3ff7_6ef7_e731_04b7),
        (0x3ff9_21fb_5444_2d18, 0x3ff2_9962_64e0_e3fe),
        (0x4086_2e42_fefa_39ef, 0x4021_d725_ed9a_197f),
        (0xc087_4910_d52d_3051, 0xc022_21be_b21c_ac5a),
        (0x412e_8480_0000_0000, 0x4059_0000_0000_0000),
        (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0x3c60_0000_0000_0001, 0x3ec0_0000_0000_0000),
        (0xc868_a161_44d6_b3cb, 0xc2c2_7972_7eff_0dc2),
        (0x4470_9a50_99e9_d2ba, 0x4170_32ce_355d_bea5),
        (0x6461_4b3f_9daf_386b, 0x4c14_b029_7b0b_3e41),
        (0xd0f1_7d23_1662_932f, 0xc59a_29b7_6ab6_5069),
        (0xea91_8e2c_d75b_9d97, 0xce24_cab7_4ca6_3783),
        (0xa598_9651_4fec_70d9, 0xb727_433e_4e83_08a9),
        (0x800b_9e26_7395_aa29, 0xaaa2_1e6d_de74_75f8),
        (0x82d2_7dc7_501f_7549, 0xab90_ca6c_9c09_9132),
        (0x42ad_3b9c_6651_f4dc, 0x40d8_a4e6_99b7_e04b),
        (0xd8ff_7d92_340d_c379, 0xc849_434d_82ba_4880),
        (0xc166_6779_17e7_48df, 0xc06c_6a29_ca10_a786),
        (0xbf9e_be4b_9e18_0c1d, 0xbfd3_e42d_c47a_786a),
        (0x3ec1_fd4e_a3ae_5b91, 0x3f8a_6907_8cd9_06b1),
        (0x401d_fb31_3190_f4ea, 0x3fff_4ff9_baf5_50e9),
        (0x3e0f_1f2c_a156_ad27, 0x3f4f_b45c_a44f_c2fc),
        (0xc076_a543_4cfe_347c, 0xc01c_8431_33b8_b5ea),
        (0xc089_0133_304f_8980, 0xc022_9148_b414_e6f2),
        (0x408e_a05f_1786_5490, 0x4023_ddb7_8476_aa0a),
        (0x407b_771f_cbef_f648, 0x401e_692c_1076_ec73),
        (0xc080_8bdb_0338_eb56, 0xc020_2e19_06cd_69de),
        (0x0000_f489_633f_1af5, 0x2a8f_83da_4227_5b03),
        (0x000c_0896_7a37_cf3c, 0x2aa2_551d_57ac_17cb),
        (0x4128_8c0a_0000_0000, 0x4057_4000_0000_0000),
    ];
    #[rustfmt::skip]
    const ATAN2: &[(u64, u64, u64)] = &[
        (0x0000_0000_0000_0000, 0x0000_0000_0000_0000, 0x0000_0000_0000_0000),
        (0x0010_0000_0000_0000, 0x3ddb_7cdf_d9d7_bdbb, 0x0222_a05f_2000_0000),
        (0x7ff0_0000_0000_0000, 0x3fe9_0000_0000_0000, 0x3ff9_21fb_5444_2d18),
        (0x3fe0_0000_0000_0000, 0x4129_21fb_5444_2d18, 0x3ea4_5f30_6dc9_c5c2),
        (0x4008_0000_0000_0000, 0x3fef_ffff_ffff_ffff, 0x3ff3_fc17_6b7a_8560),
        (0x81a5_6e1f_c2f8_f359, 0x8000_0000_0000_0001, 0xbff9_21fb_5444_2d19),
        (0xbddb_7cdf_d9d7_bdbb, 0xbddb_7cdf_d9d7_bdbc, 0xc002_d97c_7f33_21d2),
        (0xc480_f0cf_064d_d592, 0xbfe9_0000_0000_0001, 0xbff9_21fb_5444_2d19),
        (0x800c_0000_0000_0000, 0xc129_21fb_5444_2d19, 0xc009_21fb_5444_2d18),
        (0xbff9_21fb_5444_2d18, 0xbff0_0000_0000_0000, 0xc001_1a06_904d_ab99),
        (0x4005_bf0a_8b14_576a, 0x0000_0000_0000_0001, 0x3ff9_21fb_5444_2d18),
        (0x3fe5_94af_4f0d_844e, 0x4202_a05f_2000_0000, 0x3dd2_89ab_0915_2ea7),
        (0x3fe9_0000_0000_0001, 0x3fef_3333_3333_3333, 0x3fe5_9de0_b754_7892),
        (0x3fdc_0000_0000_0001, 0x412e_8480_0000_0000, 0x3e9d_5c31_593e_5da9),
        (0x4003_8000_0000_0001, 0x3fdc_0000_0000_0000, 0x3ff6_4a8c_401e_22c0),
        (0xbe20_0000_0000_0001, 0x8000_0000_0000_0002, 0xbff9_21fb_5444_2d19),
        (0xbe30_0000_0000_0001, 0xc202_a05f_2000_0001, 0xc009_21fb_5444_2d18),
        (0xc086_2e42_fefa_39f0, 0xbfef_3333_3333_3334, 0xbff9_279b_b709_7661),
        (0xc129_21fb_5444_2d19, 0xc12e_8480_0000_0001, 0xc003_9f0a_365e_996e),
        (0xc30c_6bf5_2634_0001, 0xbfdc_0000_0000_0001, 0xbff9_21fb_5444_2d1b),
        (0x433f_ffff_ffff_ffff, 0x0010_0000_0000_0000, 0x3ff9_21fb_5444_2d18),
        (0x43ef_ffff_ffff_ffff, 0x4480_f0cf_064d_d592, 0x3f5e_391d_d0f2_6a78),
        (0x3fef_fffe_ffff_ffff, 0x3fdc_0000_0000_0000, 0x3ff2_88bf_7450_4bfe),
        (0x3ff0_000a_7c5a_c471, 0x430c_6bf5_2634_0000, 0x3cd2_03bb_6d37_dc5e),
        (0x3fef_ffff_ffff_fffe, 0x3fe6_0000_0000_0000, 0x3fee_fe06_8bba_2274),
        (0xbfe5_ffff_ffff_ffff, 0x8010_0000_0000_0001, 0xbff9_21fb_5444_2d19),
        (0xc003_7fff_ffff_ffff, 0xc480_f0cf_064d_d593, 0xc009_21fb_5444_2d18),
        (0xbff0_a2b2_3f3b_ab72, 0xbfdc_0000_0000_0001, 0xbfff_8166_f1db_abe2),
        (0xc08f_ffff_ffff_ffff, 0xc30c_6bf5_2634_0001, 0xc009_21fb_5444_2416),
        (0x4090_c800_0000_0001, 0xbfe6_0000_0000_0001, 0x3ff9_249a_8ded_1ff7),
        (0x40a1_9c38_4708_fd20, 0x40e7_7526_f512_0a28, 0x3fa8_016f_4cc5_e3f6),
        (0xd7e1_6bd0_a6d4_2a1f, 0x1a74_9850_7582_2001, 0xbff9_21fb_5444_2d18),
        (0x6e15_4906_f7ef_9982, 0x4459_0838_5543_c27c, 0x3ff9_21fb_5444_2d18),
        (0x0000_0000_0000_0000, 0xbff0_0000_0000_0000, 0x4009_21fb_5444_2d18),
        (0x8000_0000_0000_0000, 0xbff0_0000_0000_0000, 0xc009_21fb_5444_2d18),
        (0x3ff0_0000_0000_0000, 0x0000_0000_0000_0000, 0x3ff9_21fb_5444_2d18),
        (0xbff0_0000_0000_0000, 0x8000_0000_0000_0000, 0xbff9_21fb_5444_2d18),
        (0x7ff0_0000_0000_0000, 0x7ff0_0000_0000_0000, 0x3fe9_21fb_5444_2d18),
        (0xfff0_0000_0000_0000, 0xfff0_0000_0000_0000, 0xc002_d97c_7f33_21d2),
        (0x7ff0_0000_0000_0000, 0xfff0_0000_0000_0000, 0x4002_d97c_7f33_21d2),
        (0x3ff0_0000_0000_0000, 0x7ff0_0000_0000_0000, 0x0000_0000_0000_0000),
        (0xbff0_0000_0000_0000, 0xfff0_0000_0000_0000, 0xc009_21fb_5444_2d18),
        (0x7e37_e43c_8800_759c, 0x01a5_6e1f_c2f8_f359, 0x3ff9_21fb_5444_2d18),
        (0x01a5_6e1f_c2f8_f359, 0xfe37_e43c_8800_759c, 0x4009_21fb_5444_2d18),
        (0x4008_0000_0000_0000, 0x3ff0_0000_0000_0000, 0x3ff3_fc17_6b7a_8560),
        (0xc000_0000_0000_0000, 0xc014_0000_0000_0000, 0xc006_16b4_66d7_3d60),
        (0x3fe0_0000_0000_0000, 0xbfd0_0000_0000_0000, 0x4000_468a_8ace_4df6),
        (0x7ff8_0000_0000_0000, 0x3ff0_0000_0000_0000, 0x7ff8_0000_0000_0000),
    ];
    #[rustfmt::skip]
    const POW: &[(u64, u64, u64)] = &[
        (0xc028_0000_0000_0000, 0xc028_0000_0000_0000, 0x3d3f_91bd_1b62_b9cf),
        (0xc01c_0000_0000_0000, 0x4005_bf0a_8b14_5769, 0xfff8_0000_0000_0000),
        (0xbff0_0000_0000_0000, 0xc090_c800_0000_0000, 0x3ff0_0000_0000_0000),
        (0x4014_0000_0000_0000, 0x43b0_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0x4026_0000_0000_0000, 0x3ff0_0000_0000_0001, 0x4026_0000_0000_0003),
        (0x3ff8_0000_0000_0000, 0x0000_0000_0000_0001, 0x3ff0_0000_0000_0000),
        (0x3fd5_5555_5555_5555, 0x4024_0000_0000_0000, 0x3ef1_c1fa_5f67_8882),
        (0x3fb9_9999_9999_999a, 0xbff8_0000_0000_0000, 0x403f_9f6e_4990_f226),
        (0x3ee4_f8b5_88e3_68f1, 0x401c_0000_0000_0000, 0x38aa_95a5_b7f8_7a13),
        (0x7e37_e43c_8800_759c, 0xc014_0000_0000_0000, 0x0000_0000_0000_0000),
        (0xffef_ffff_ffff_ffff, 0x41e6_5a0b_c000_0000, 0x7ff0_0000_0000_0000),
        (0xfff8_0000_0000_0000, 0xbff0_0001_0000_0000, 0xfff8_0000_0000_0000),
        (0xbfef_ffff_ffff_ffff, 0xc090_c800_0000_0000, 0x3ff0_0000_0000_0219),
        (0xc340_0000_0000_0000, 0xc1e0_0000_0020_0000, 0x8000_0000_0000_0000),
        (0xc08f_f800_0000_0000, 0x7ff0_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0xc090_cc00_0000_0000, 0xc0f8_6a00_0000_0000, 0x0000_0000_0000_0000),
        (0x4090_cc00_0000_0000, 0x3fd5_5555_5555_5555, 0x4024_7ced_50bc_2233),
        (0xbfef_ffff_ca50_1acb, 0xbfe0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0xc009_21fb_5444_2d18, 0x4000_0000_0000_0000, 0x4023_bd3c_c9be_45de),
        (0x8010_0000_0000_0000, 0xc024_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0x4018_6a02_1a84_75ca, 0xc010_c41e_fcb3_2a5a, 0x3f40_b2bc_d90f_474c),
        (0x4004_429e_d8fe_128f, 0xc032_550f_4454_55db, 0x3e65_77ef_ec0b_db58),
        (0x4002_e2cb_6f37_dfae, 0xc010_c09d_24de_d3d8, 0x3f9c_0ce0_5cb3_b036),
        (0x50fe_21ae_5150_73fe, 0xbfbc_b173_2010_defc, 0x3e05_46ac_cff0_35fe),
        (0x0eb6_19a3_a3be_0e97, 0x23e6_7030_14e8_6170, 0x3ff0_0000_0000_0000),
        (0xf457_5646_ea36_90f6, 0x166e_e1d5_3456_b27e, 0xfff8_0000_0000_0000),
        (0xc03c_0000_0000_0000, 0xc022_0000_0000_0000, 0xbd3a_9bbb_147e_0dd9),
        (0x3fef_ffff_ffff_ff9b, 0x42dc_8b2b_bf6d_b554, 0x3fcf_52c0_a697_8b59),
        (0x3fef_ffff_6c79_2034, 0x4312_f47a_48c4_f4f8, 0x0000_0000_0000_0000),
        (0x3ffd_e6a1_21bd_de40, 0xc091_188e_e063_ae52, 0x023f_16c3_afd9_8fcb),
        (0x3fe0_0000_0000_0000, 0x407e_8c42_261b_daec, 0x2162_d0c5_2a71_cfe8),
        (0x4000_0000_0000_0000, 0xc08e_76c7_e5b9_e152, 0x0301_c858_1035_305b),
        (0x0000_0000_0000_1b61, 0x3fe4_7d78_aa50_8d40, 0x1576_5210_8096_9d3c),
        (0xc000_0000_0000_0000, 0x4008_0000_0000_0000, 0xc020_0000_0000_0000),
        (0xc000_0000_0000_0000, 0xc008_0000_0000_0000, 0xbfc0_0000_0000_0000),
        (0xc020_0000_0000_0000, 0x3fd5_5555_5555_5555, 0xfff8_0000_0000_0000),
        (0x4000_0000_0000_0000, 0xc090_c800_0000_0000, 0x0000_0000_0000_0001),
        (0x4000_0000_0000_0000, 0xc090_cc00_0000_0000, 0x0000_0000_0000_0000),
        (0x4000_0000_0000_0000, 0x408f_f800_0000_0000, 0x7fe0_0000_0000_0000),
        (0x4000_0000_0000_0000, 0x4090_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0x8000_0000_0000_0000, 0xbff0_0000_0000_0000, 0xfff0_0000_0000_0000),
        (0x8000_0000_0000_0000, 0xc000_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0x0000_0000_0000_0000, 0xbfe0_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0xbff0_0000_0000_0000, 0x7ff0_0000_0000_0000, 0xfff8_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0x4008_0000_0000_0000, 0xfff0_0000_0000_0000),
        (0xfff0_0000_0000_0000, 0x3fe0_0000_0000_0000, 0x7ff0_0000_0000_0000),
        (0x3ff0_0000_1ad7_f29b, 0x41e6_5a0b_c000_0000, 0x5afc_05a5_6d67_53a4),
        (0x3fef_ffff_ca50_1acb, 0xc1e6_5a0b_c000_0000, 0x5afc_05db_e8fc_c6d2),
        (0x4024_0000_0000_0000, 0xc014_0000_0000_0000, 0x3ee4_f8b5_88e3_68f0),
        (0x4024_0000_0000_0000, 0x4036_0000_0000_0000, 0x4480_f0cf_064d_d592),
        (0x4008_0000_0000_0000, 0x3fe0_0000_0000_0000, 0x3ffb_b67a_e858_4caa),
        (0x0000_1268_8b70_e62b, 0x3fe8_0000_0000_0000, 0x0fa9_22f8_9593_5598),
        (0x3fe0_0000_0000_0000, 0x4090_ca00_0000_0000, 0x0000_0000_0000_0001),
        (0x7ff8_0000_0000_0000, 0x0000_0000_0000_0000, 0x3ff0_0000_0000_0000),
        (0x3ff0_0000_0000_0000, 0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        (0xc008_0000_0000_0000, 0x4376_3457_85d8_a000, 0x7ff0_0000_0000_0000),
        (0x3ff8_0000_0000_0000, 0xc085_e200_0000_0000, 0x2654_d254_3a1e_b14e),
    ];

    #[test]
    fn sin_matches_java() {
        check1("sin", sin, SIN);
    }

    #[test]
    fn cos_matches_java() {
        check1("cos", cos, COS);
    }

    #[test]
    fn tan_matches_java() {
        check1("tan", tan, TAN);
    }

    #[test]
    fn asin_matches_java() {
        check1("asin", asin, ASIN);
    }

    #[test]
    fn acos_matches_java() {
        check1("acos", acos, ACOS);
    }

    #[test]
    fn atan_matches_java() {
        check1("atan", atan, ATAN);
    }

    #[test]
    fn exp_matches_java() {
        check1("exp", exp, EXP);
    }

    #[test]
    fn log_matches_java() {
        check1("log", log, LOG);
    }

    #[test]
    fn log10_matches_java() {
        check1("log10", log10, LOG10);
    }

    #[test]
    fn cbrt_matches_java() {
        check1("cbrt", cbrt, CBRT);
    }

    #[test]
    fn atan2_matches_java() {
        check2("atan2", atan2, ATAN2);
    }

    #[test]
    fn pow_matches_java() {
        check2("pow", pow, POW);
    }
}
