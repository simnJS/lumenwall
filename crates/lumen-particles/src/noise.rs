//! 3D gradient noise ("improved noise", Ken Perlin 2002) and curl noise (spec 05 §9.3, §9.4).

use glam::Vec3;

const PERM: [u8; 256] = [
    151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225, 140, 36, 103, 30, 69,
    142, 8, 99, 37, 240, 21, 10, 23, 190, 6, 148, 247, 120, 234, 75, 0, 26, 197, 62, 94, 252, 219,
    203, 117, 35, 11, 32, 57, 177, 33, 88, 237, 149, 56, 87, 174, 20, 125, 136, 171, 168, 68, 175,
    74, 165, 71, 134, 139, 48, 27, 166, 77, 146, 158, 231, 83, 111, 229, 122, 60, 211, 133, 230,
    220, 105, 92, 41, 55, 46, 245, 40, 244, 102, 143, 54, 65, 25, 63, 161, 1, 216, 80, 73, 209, 76,
    132, 187, 208, 89, 18, 169, 200, 196, 135, 130, 116, 188, 159, 86, 164, 100, 109, 198, 173,
    186, 3, 64, 52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126, 255, 82, 85, 212, 207, 206,
    59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213, 119, 248, 152, 2, 44, 154, 163,
    70, 221, 153, 101, 155, 167, 43, 172, 9, 129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232,
    178, 185, 112, 104, 218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144, 12, 191, 179, 162,
    241, 81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157, 184, 84, 204,
    176, 115, 121, 50, 45, 127, 4, 150, 254, 138, 236, 205, 93, 222, 114, 67, 29, 24, 72, 243, 141,
    128, 195, 78, 66, 215, 61, 156, 180,
];

/// Permutation repeated to 512 entries (`P[i + 256] = P[i]`).
static P: [u8; 512] = {
    let mut t = [0u8; 512];
    let mut i = 0;
    while i < 512 {
        t[i] = PERM[i & 255];
        i += 1;
    }
    t
};

/// Gradient of hash `h` as coefficients on (x, y, z): `a = x if h < 8 else y`;
/// `b = y if h < 4, x if h ∈ {12, 14}, else z`; `a` negated if bit 0, `b` negated if bit 1.
/// `gx·x + gy·y + gz·z` equals `±a ± b` exactly (up to the sign of a zero), without branches.
static GRAD: [[f64; 3]; 16] = {
    let mut g = [[0.0f64; 3]; 16];
    let mut h = 0;
    while h < 16 {
        let a_axis = if h < 8 { 0 } else { 1 };
        let b_axis = if h < 4 {
            1
        } else if h == 12 || h == 14 {
            0
        } else {
            2
        };
        g[h][a_axis] = if h & 1 != 0 { -1.0 } else { 1.0 };
        g[h][b_axis] = if h & 2 != 0 { -1.0 } else { 1.0 };
        h += 1;
    }
    g
};

#[inline(always)]
fn grad(hash: u8, x: f64, y: f64, z: f64) -> f64 {
    let g = &GRAD[usize::from(hash & 15)];
    g[0] * x + g[1] * y + g[2] * z
}

#[inline(always)]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline(always)]
fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// `floor` without a libm call (x86-64 baseline has no `roundsd`): equal to `f64::floor` except
/// that `-0.0` maps to `+0.0`, which only flips the sign of zero fractions.
#[inline(always)]
fn floor(x: f64) -> f64 {
    // |x| >= 2^52 (or NaN): already integral.
    if x.is_nan() || x.abs() >= 4_503_599_627_370_496.0 {
        return x;
    }
    let t = (x as i64) as f64;
    // Branchless: the sign of the coordinates is unpredictable per particle.
    t - f64::from(u8::from(t > x))
}

#[inline(always)]
fn cell(v: f64) -> (usize, f64) {
    let f = floor(v);
    (((f as i64) & 255) as usize, v - f)
}

/// Scalar improved noise, evaluated in `f64`.
#[inline]
pub fn noise(x: f64, y: f64, z: f64) -> f64 {
    let (xi, x) = cell(x);
    let (yi, y) = cell(y);
    let (zi, z) = cell(z);
    let u = fade(x);
    let v = fade(y);
    let w = fade(z);
    let a = usize::from(P[xi]) + yi;
    let aa = usize::from(P[a]) + zi;
    let ab = usize::from(P[a + 1]) + zi;
    let b = usize::from(P[xi + 1]) + yi;
    let ba = usize::from(P[b]) + zi;
    let bb = usize::from(P[b + 1]) + zi;
    lerp(
        w,
        lerp(
            v,
            lerp(u, grad(P[aa], x, y, z), grad(P[ba], x - 1.0, y, z)),
            lerp(
                u,
                grad(P[ab], x, y - 1.0, z),
                grad(P[bb], x - 1.0, y - 1.0, z),
            ),
        ),
        lerp(
            v,
            lerp(
                u,
                grad(P[aa + 1], x, y, z - 1.0),
                grad(P[ba + 1], x - 1.0, y, z - 1.0),
            ),
            lerp(
                u,
                grad(P[ab + 1], x, y - 1.0, z - 1.0),
                grad(P[bb + 1], x - 1.0, y - 1.0, z - 1.0),
            ),
        ),
    )
}

/// Offsets of the three channels of the vector noise, added in `f64`.
const OFFSETS: [[f64; 3]; 3] = [[0.0, 0.0, 0.0], [89.2, 33.1, 57.3], [100.3, 120.1, 142.2]];

/// Channel `c` of the vector noise `N(p)`, result converted to `f32`.
#[inline(always)]
fn channel(p: Vec3, c: usize) -> f32 {
    let o = &OFFSETS[c];
    noise(
        f64::from(p.x) + o[0],
        f64::from(p.y) + o[1],
        f64::from(p.z) + o[2],
    ) as f32
}

/// Vector noise `N(p) = (n(p), n(p + o1), n(p + o2))`.
pub fn noise3(p: Vec3) -> Vec3 {
    Vec3::new(channel(p, 0), channel(p, 1), channel(p, 2))
}

/// Noise at four points known to lie in the lattice cell `(xi, yi, zi)`, given their fractional
/// parts. Point by point this is exactly the arithmetic of [`noise`]; only the hashing and the
/// gradient lookups are shared.
#[inline]
fn noise4_in_cell(
    xi: usize,
    yi: usize,
    zi: usize,
    fx: [f64; 4],
    fy: [f64; 4],
    fz: [f64; 4],
) -> [f64; 4] {
    let a = usize::from(P[xi]) + yi;
    let aa = usize::from(P[a]) + zi;
    let ab = usize::from(P[a + 1]) + zi;
    let b = usize::from(P[xi + 1]) + yi;
    let ba = usize::from(P[b]) + zi;
    let bb = usize::from(P[b + 1]) + zi;
    let g = |i: usize| &GRAD[usize::from(P[i] & 15)];
    let (g0, g1, g2, g3) = (g(aa), g(ba), g(ab), g(bb));
    let (g4, g5, g6, g7) = (g(aa + 1), g(ba + 1), g(ab + 1), g(bb + 1));
    // Lane-wise (structure-of-arrays) so the compiler can pair the four points in SIMD registers;
    // each lane performs exactly the scalar operations of `noise`, in the same order.
    type L = [f64; 4];
    #[inline(always)]
    fn dot(g: &[f64; 3], x: &L, y: &L, z: &L) -> L {
        [0, 1, 2, 3].map(|k| g[0] * x[k] + g[1] * y[k] + g[2] * z[k])
    }
    #[inline(always)]
    fn mix(t: &L, a: L, b: L) -> L {
        [0, 1, 2, 3].map(|k| a[k] + t[k] * (b[k] - a[k]))
    }
    let (x1, y1, z1) = (
        fx.map(|v| v - 1.0),
        fy.map(|v| v - 1.0),
        fz.map(|v| v - 1.0),
    );
    let (u, v, w) = (fx.map(fade), fy.map(fade), fz.map(fade));
    let x00 = mix(&u, dot(g0, &fx, &fy, &fz), dot(g1, &x1, &fy, &fz));
    let x10 = mix(&u, dot(g2, &fx, &y1, &fz), dot(g3, &x1, &y1, &fz));
    let x01 = mix(&u, dot(g4, &fx, &fy, &z1), dot(g5, &x1, &fy, &z1));
    let x11 = mix(&u, dot(g6, &fx, &y1, &z1), dot(g7, &x1, &y1, &z1));
    mix(&w, mix(&v, x00, x10), mix(&v, x01, x11))
}

/// Channel `c` of the vector noise at four points (bit-identical to four [`channel`] calls).
#[inline]
fn channel4(pts: [Vec3; 4], c: usize) -> [f32; 4] {
    let o = &OFFSETS[c];
    let x = pts.map(|p| f64::from(p.x) + o[0]);
    let y = pts.map(|p| f64::from(p.y) + o[1]);
    let z = pts.map(|p| f64::from(p.z) + o[2]);
    let (flx, fly, flz) = (x.map(floor), y.map(floor), z.map(floor));
    let same = |f: &[f64; 4]| f[1] == f[0] && f[2] == f[0] && f[3] == f[0];
    if same(&flx) && same(&fly) && same(&flz) {
        let (xi, _) = cell(x[0]);
        let (yi, _) = cell(y[0]);
        let (zi, _) = cell(z[0]);
        let fr = |v: [f64; 4], f: [f64; 4]| [v[0] - f[0], v[1] - f[1], v[2] - f[2], v[3] - f[3]];
        noise4_in_cell(xi, yi, zi, fr(x, flx), fr(y, fly), fr(z, flz)).map(|v| v as f32)
    } else {
        // A sample crossed a cell boundary: evaluate each point on its own.
        [0, 1, 2, 3].map(|k| noise(x[k], y[k], z[k]) as f32)
    }
}

const EPS: f32 = 1e-4;

/// Curl of the vector noise by `f32` central differences with `ε = 1e-4`.
///
/// Only the 12 channel samples that enter the curl are evaluated (the reference evaluates the full
/// vector noise at 6 points, 18 samples; the 6 extra ones are pure and unused), four per channel
/// with shared lattice hashing. The result is bit-identical to the 18-sample formulation.
pub fn curl(p: Vec3) -> Vec3 {
    let px = Vec3::new(p.x + EPS, p.y, p.z);
    let mx = Vec3::new(p.x - EPS, p.y, p.z);
    let py = Vec3::new(p.x, p.y + EPS, p.z);
    let my = Vec3::new(p.x, p.y - EPS, p.z);
    let pz = Vec3::new(p.x, p.y, p.z + EPS);
    let mz = Vec3::new(p.x, p.y, p.z - EPS);
    let inv = 2.0 * EPS;
    let n0 = channel4([py, my, pz, mz], 0);
    let n1 = channel4([px, mx, pz, mz], 1);
    let n2 = channel4([px, mx, py, my], 2);
    let (dx_dy, dx_dz) = ((n0[0] - n0[1]) / inv, (n0[2] - n0[3]) / inv);
    let (dy_dx, dy_dz) = ((n1[0] - n1[1]) / inv, (n1[2] - n1[3]) / inv);
    let (dz_dx, dz_dy) = ((n2[0] - n2[1]) / inv, (n2[2] - n2[3]) / inv);
    Vec3::new(dz_dy - dy_dz, dx_dz - dz_dx, dy_dx - dx_dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Literal transcription of the gradient rule of §9.3, used to check the table.
    fn grad_rule(h: u8, x: f64, y: f64, z: f64) -> f64 {
        let h = h & 15;
        let a = if h < 8 { x } else { y };
        let b = if h < 4 {
            y
        } else if h == 12 || h == 14 {
            x
        } else {
            z
        };
        (if h & 1 != 0 { -a } else { a }) + (if h & 2 != 0 { -b } else { b })
    }

    #[test]
    fn gradient_table_matches_rule() {
        for h in 0..16u8 {
            for &(x, y, z) in &[(0.3, -0.7, 0.11), (-1.25, 0.5, 0.875), (0.0, 1.0, -1.0)] {
                assert_eq!(grad(h, x, y, z), grad_rule(h, x, y, z), "h = {h}");
            }
        }
    }

    #[test]
    fn fast_floor() {
        for &x in &[
            0.0,
            0.5,
            -0.5,
            -1.0,
            1.0,
            -1e-300,
            2.999_999,
            -2.000_001,
            1e15 + 0.5,
            -1e15 - 0.5,
            1e300,
            -1e300,
        ] {
            assert_eq!(floor(x), x.floor(), "{x}");
        }
        assert!(floor(f64::NAN).is_nan());
        assert_eq!(floor(f64::NEG_INFINITY), f64::NEG_INFINITY);
    }

    #[test]
    fn noise_known_values() {
        // Integer lattice points are zero.
        assert_eq!(noise(0.0, 0.0, 0.0), 0.0);
        assert_eq!(noise(3.0, -7.0, 12.0), 0.0);
        // Hand-computed along the x axis (y = z = 0 so v = w = 0): only the corners (0,0,0) and
        // (1,0,0) contribute. A = P[0] = 151, AA = P[151] = 17, corner hash P[17] = 36 → h 4: x + z.
        // B = P[1] = 160, BA = P[160] = 119, corner hash P[119] = 86 → h 6: x − z.
        assert_eq!((P[0], P[151], P[17]), (151, 17, 36));
        assert_eq!((P[1], P[160], P[119]), (160, 119, 86));
        // x = 0.5: u = 0.5, lerp(0.5, 0.5, −0.5) = 0.
        assert_eq!(noise(0.5, 0.0, 0.0), 0.0);
        // x = 0.25: u = fade(0.25) = 53/512; lerp(u, 0.25, −0.75) = 0.25 − 53/512 = 0.146484375.
        assert_eq!(noise(0.25, 0.0, 0.0), 0.146_484_375);
        // Range sanity.
        let mut max = 0.0f64;
        for i in 0..2000 {
            let t = f64::from(i) * 0.137;
            max = max.max(noise(t, t * 0.71 + 3.1, -t * 1.3).abs());
        }
        assert!(max > 0.2 && max <= 1.1);
    }

    #[test]
    fn curl_matches_the_full_formulation_bit_for_bit() {
        // §9.4 literally: full vector noise at 6 points (18 samples).
        let full = |p: Vec3| {
            let d = |a: Vec3, b: Vec3| (noise3(a) - noise3(b)) / (2.0 * EPS);
            let ddx = d(
                Vec3::new(p.x + EPS, p.y, p.z),
                Vec3::new(p.x - EPS, p.y, p.z),
            );
            let ddy = d(
                Vec3::new(p.x, p.y + EPS, p.z),
                Vec3::new(p.x, p.y - EPS, p.z),
            );
            let ddz = d(
                Vec3::new(p.x, p.y, p.z + EPS),
                Vec3::new(p.x, p.y, p.z - EPS),
            );
            Vec3::new(ddy.z - ddz.y, ddz.x - ddx.z, ddx.y - ddy.x)
        };
        let c = curl(Vec3::new(0.3, 1.7, -2.2));
        assert!(c.is_finite() && c.length() > 1e-3);
        let mut points = vec![
            // Samples straddling cell boundaries (with and without the channel offsets).
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(-0.00005, 0.5, 0.5),
            Vec3::new(0.5, 0.5, 0.00005),
            Vec3::new(-89.2 + 0.00003, 7.0, 1.0),
            Vec3::new(2000.0, -3000.0, 0.25),
        ];
        for i in 0..500 {
            let t = i as f32 * 0.731;
            points.push(Vec3::new(
                t.sin() * 300.0,
                t * 0.37 - 50.0,
                (t * 1.3).cos() * 40.0,
            ));
        }
        for p in points {
            assert_eq!(
                curl(p).to_array().map(f32::to_bits),
                full(p).to_array().map(f32::to_bits),
                "{p}"
            );
        }
    }
}
