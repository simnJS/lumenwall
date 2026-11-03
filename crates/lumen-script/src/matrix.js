// Lumen SceneScript matrices: Mat3, Mat4 (WE lib.sceneScript.d.ts).
// Compiled on first use of `Mat3`/`Mat4` (it is the largest piece of JS in the runtime).
// Matrices are column-major: m[column * size + row], like GLSL.
(function (natives) {
  'use strict';
  const { Vec2, Vec3, Vec4, fmt } = natives;
  const EPSILON = 1e-5;
  const D2R = Math.PI / 180;
  const R2D = 180 / Math.PI;
  const num = (v, d) => (typeof v === 'number' ? v : d);
  const comp = (v, k, d) => (v !== null && typeof v === 'object' ? num(v[k], d) : d);
  const formatArray = (m) => m.map(fmt).join(' ');

  function mul(a, b, n) {
    const r = new Array(n * n);
    for (let c = 0; c < n; c++) {
      for (let row = 0; row < n; row++) {
        let s = 0;
        for (let k = 0; k < n; k++) s += a[k * n + row] * b[c * n + k];
        r[c * n + row] = s;
      }
    }
    return r;
  }

  function equalArrays(a, b) {
    for (let i = 0; i < a.length; i++) if (!(Math.abs(a[i] - b[i]) <= EPSILON)) return false;
    return true;
  }

  // Euler angles in degrees, applied X then Y then Z: R = Rz * Ry * Rx.
  function eulerRotation3(x, y, z) {
    const [sx, cx] = [Math.sin(x * D2R), Math.cos(x * D2R)];
    const [sy, cy] = [Math.sin(y * D2R), Math.cos(y * D2R)];
    const [sz, cz] = [Math.sin(z * D2R), Math.cos(z * D2R)];
    // Column-major 3x3.
    return [
      cz * cy, sz * cy, -sy,
      cz * sy * sx - sz * cx, sz * sy * sx + cz * cx, cy * sx,
      cz * sy * cx + sz * sx, sz * sy * cx - cz * sx, cy * cx,
    ];
  }

  // Inverse of eulerRotation3 for a rotation matrix given as column-major 3x3.
  function extractEuler3(r) {
    const sy = Math.max(-1, Math.min(1, -r[2]));
    const y = Math.asin(sy);
    let x;
    let z;
    if (Math.abs(Math.cos(y)) > 1e-6) {
      x = Math.atan2(r[5], r[8]);
      z = Math.atan2(r[1], r[0]);
    } else {
      x = Math.atan2(-r[7], r[4]);
      z = 0;
    }
    return new Vec3(x * R2D, y * R2D, z * R2D);
  }

  class Mat4 {
    constructor() {
      this.m = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
    }

    static __fromArray(a) {
      const r = new Mat4();
      for (let i = 0; i < 16; i++) r.m[i] = num(a[i], 0);
      return r;
    }

    static identity() {
      return new Mat4();
    }

    static fromTranslation(v) {
      const r = new Mat4();
      r.m[12] = comp(v, 'x', 0);
      r.m[13] = comp(v, 'y', 0);
      r.m[14] = comp(v, 'z', 0);
      return r;
    }

    static fromScale(v) {
      const r = new Mat4();
      if (typeof v === 'number') {
        r.m[0] = r.m[5] = r.m[10] = v;
      } else {
        r.m[0] = comp(v, 'x', 1);
        r.m[5] = comp(v, 'y', 1);
        r.m[10] = comp(v, 'z', 1);
      }
      return r;
    }

    static fromRotation(angle, axis) {
      const r = new Mat4();
      let x = comp(axis, 'x', 0);
      let y = comp(axis, 'y', 0);
      let z = comp(axis, 'z', 0);
      const len = Math.hypot(x, y, z);
      if (len === 0) return r;
      x /= len;
      y /= len;
      z /= len;
      const a = num(angle, 0) * D2R;
      const c = Math.cos(a);
      const s = Math.sin(a);
      const t = 1 - c;
      const m = r.m;
      m[0] = t * x * x + c;
      m[1] = t * x * y + s * z;
      m[2] = t * x * z - s * y;
      m[4] = t * x * y - s * z;
      m[5] = t * y * y + c;
      m[6] = t * y * z + s * x;
      m[8] = t * x * z + s * y;
      m[9] = t * y * z - s * x;
      m[10] = t * z * z + c;
      return r;
    }

    static fromEuler(x, y, z) {
      if (x !== null && typeof x === 'object') {
        z = comp(x, 'z', 0);
        y = comp(x, 'y', 0);
        x = comp(x, 'x', 0);
      }
      const e = eulerRotation3(num(x, 0), num(y, 0), num(z, 0));
      const r = new Mat4();
      for (let c = 0; c < 3; c++) for (let row = 0; row < 3; row++) r.m[c * 4 + row] = e[c * 3 + row];
      return r;
    }

    static fromBasis(right, up, forward) {
      const r = new Mat4();
      [right, up, forward].forEach((v, c) => {
        r.m[c * 4] = comp(v, 'x', 0);
        r.m[c * 4 + 1] = comp(v, 'y', 0);
        r.m[c * 4 + 2] = comp(v, 'z', 0);
      });
      return r;
    }

    // Right-handed view matrix (GLSL/glm lookAt convention).
    static lookAt(eye, center, up) {
      const e = [comp(eye, 'x', 0), comp(eye, 'y', 0), comp(eye, 'z', 0)];
      const c = [comp(center, 'x', 0), comp(center, 'y', 0), comp(center, 'z', 0)];
      const u = [comp(up, 'x', 0), comp(up, 'y', 1), comp(up, 'z', 0)];
      const norm = (v) => {
        const l = Math.hypot(v[0], v[1], v[2]) || 1;
        return [v[0] / l, v[1] / l, v[2] / l];
      };
      const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
      const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
      const f = norm([c[0] - e[0], c[1] - e[1], c[2] - e[2]]);
      const s = norm(cross(f, u));
      const v = cross(s, f);
      return Mat4.__fromArray([
        s[0], v[0], -f[0], 0,
        s[1], v[1], -f[1], 0,
        s[2], v[2], -f[2], 0,
        -dot(s, e), -dot(v, e), dot(f, e), 1,
      ]);
    }

    static compose(translation, rotation, scale) {
      return Mat4.fromTranslation(translation).multiply(Mat4.fromEuler(rotation)).multiply(Mat4.fromScale(scale));
    }

    translation(position) {
      if (position !== undefined) {
        this.m[12] = comp(position, 'x', this.m[12]);
        this.m[13] = comp(position, 'y', this.m[13]);
        this.m[14] = comp(position, 'z', this.m[14]);
      }
      return new Vec3(this.m[12], this.m[13], this.m[14]);
    }

    right() {
      return new Vec3(this.m[0], this.m[1], this.m[2]);
    }

    up() {
      return new Vec3(this.m[4], this.m[5], this.m[6]);
    }

    forward() {
      return new Vec3(this.m[8], this.m[9], this.m[10]);
    }

    add(other) {
      return Mat4.__fromArray(this.m.map((x, i) => x + other.m[i]));
    }

    subtract(other) {
      return Mat4.__fromArray(this.m.map((x, i) => x - other.m[i]));
    }

    multiply(value) {
      if (value instanceof Mat4) return Mat4.__fromArray(mul(this.m, value.m, 4));
      if (typeof value === 'number') return Mat4.__fromArray(this.m.map((x) => x * value));
      const v = [comp(value, 'x', 0), comp(value, 'y', 0), comp(value, 'z', 0), comp(value, 'w', 1)];
      const r = [0, 0, 0, 0];
      for (let row = 0; row < 4; row++) {
        for (let k = 0; k < 4; k++) r[row] += this.m[k * 4 + row] * v[k];
      }
      return new Vec4(r[0], r[1], r[2], r[3]);
    }

    translate(v) {
      return this.multiply(Mat4.fromTranslation(v));
    }

    rotate(angle, axis) {
      return this.multiply(Mat4.fromRotation(angle, axis));
    }

    scale(v) {
      return this.multiply(Mat4.fromScale(v));
    }

    transformPoint(v) {
      const r = this.multiply(new Vec4(comp(v, 'x', 0), comp(v, 'y', 0), comp(v, 'z', 0), 1));
      return new Vec3(r.x, r.y, r.z);
    }

    transformDirection(v) {
      const r = this.multiply(new Vec4(comp(v, 'x', 0), comp(v, 'y', 0), comp(v, 'z', 0), 0));
      return new Vec3(r.x, r.y, r.z);
    }

    transpose() {
      const r = new Mat4();
      for (let c = 0; c < 4; c++) for (let row = 0; row < 4; row++) r.m[c * 4 + row] = this.m[row * 4 + c];
      return r;
    }

    determinant() {
      const a = this.m;
      const b00 = a[0] * a[5] - a[1] * a[4];
      const b01 = a[0] * a[6] - a[2] * a[4];
      const b02 = a[0] * a[7] - a[3] * a[4];
      const b03 = a[1] * a[6] - a[2] * a[5];
      const b04 = a[1] * a[7] - a[3] * a[5];
      const b05 = a[2] * a[7] - a[3] * a[6];
      const b06 = a[8] * a[13] - a[9] * a[12];
      const b07 = a[8] * a[14] - a[10] * a[12];
      const b08 = a[8] * a[15] - a[11] * a[12];
      const b09 = a[9] * a[14] - a[10] * a[13];
      const b10 = a[9] * a[15] - a[11] * a[13];
      const b11 = a[10] * a[15] - a[11] * a[14];
      return b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
    }

    inverse() {
      const a = this.m;
      const b00 = a[0] * a[5] - a[1] * a[4];
      const b01 = a[0] * a[6] - a[2] * a[4];
      const b02 = a[0] * a[7] - a[3] * a[4];
      const b03 = a[1] * a[6] - a[2] * a[5];
      const b04 = a[1] * a[7] - a[3] * a[5];
      const b05 = a[2] * a[7] - a[3] * a[6];
      const b06 = a[8] * a[13] - a[9] * a[12];
      const b07 = a[8] * a[14] - a[10] * a[12];
      const b08 = a[8] * a[15] - a[11] * a[12];
      const b09 = a[9] * a[14] - a[10] * a[13];
      const b10 = a[9] * a[15] - a[11] * a[13];
      const b11 = a[10] * a[15] - a[11] * a[14];
      const det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
      const d = 1 / det;
      return Mat4.__fromArray([
        (a[5] * b11 - a[6] * b10 + a[7] * b09) * d,
        (a[2] * b10 - a[1] * b11 - a[3] * b09) * d,
        (a[13] * b05 - a[14] * b04 + a[15] * b03) * d,
        (a[10] * b04 - a[9] * b05 - a[11] * b03) * d,
        (a[6] * b08 - a[4] * b11 - a[7] * b07) * d,
        (a[0] * b11 - a[2] * b08 + a[3] * b07) * d,
        (a[14] * b02 - a[12] * b05 - a[15] * b01) * d,
        (a[8] * b05 - a[10] * b02 + a[11] * b01) * d,
        (a[4] * b10 - a[5] * b08 + a[7] * b06) * d,
        (a[1] * b08 - a[0] * b10 - a[3] * b06) * d,
        (a[12] * b04 - a[13] * b02 + a[15] * b00) * d,
        (a[9] * b02 - a[8] * b04 - a[11] * b00) * d,
        (a[5] * b07 - a[4] * b09 - a[6] * b06) * d,
        (a[0] * b09 - a[1] * b07 + a[2] * b06) * d,
        (a[13] * b01 - a[12] * b03 - a[14] * b00) * d,
        (a[8] * b03 - a[9] * b01 + a[10] * b00) * d,
      ]);
    }

    __scaleAndRotation() {
      const m = this.m;
      const sx0 = Math.hypot(m[0], m[1], m[2]);
      const sy = Math.hypot(m[4], m[5], m[6]);
      const sz = Math.hypot(m[8], m[9], m[10]);
      const sx = this.determinant() < 0 ? -sx0 : sx0;
      const r = [
        m[0] / (sx || 1), m[1] / (sx || 1), m[2] / (sx || 1),
        m[4] / (sy || 1), m[5] / (sy || 1), m[6] / (sy || 1),
        m[8] / (sz || 1), m[9] / (sz || 1), m[10] / (sz || 1),
      ];
      return [new Vec3(sx, sy, sz), r];
    }

    extractEuler() {
      return extractEuler3(this.__scaleAndRotation()[1]);
    }

    normalMatrix() {
      const a = this.m;
      const r = Mat3.__fromArray([a[0], a[1], a[2], a[4], a[5], a[6], a[8], a[9], a[10]]);
      return r.inverse().transpose();
    }

    decompose() {
      const [scale, r] = this.__scaleAndRotation();
      return { translation: new Vec3(this.m[12], this.m[13], this.m[14]), rotation: extractEuler3(r), scale };
    }

    copy() {
      return Mat4.__fromArray(this.m);
    }

    equals(other) {
      return other instanceof Mat4 && equalArrays(this.m, other.m);
    }

    toString() {
      return formatArray(this.m);
    }
  }

  class Mat3 {
    constructor() {
      this.m = [1, 0, 0, 0, 1, 0, 0, 0, 1];
    }

    static __fromArray(a) {
      const r = new Mat3();
      for (let i = 0; i < 9; i++) r.m[i] = num(a[i], 0);
      return r;
    }

    static identity() {
      return new Mat3();
    }

    static fromTranslation(v) {
      const r = new Mat3();
      r.m[6] = comp(v, 'x', 0);
      r.m[7] = comp(v, 'y', 0);
      return r;
    }

    static fromScale(v) {
      const r = new Mat3();
      if (typeof v === 'number') {
        r.m[0] = r.m[4] = v;
      } else {
        r.m[0] = comp(v, 'x', 1);
        r.m[4] = comp(v, 'y', 1);
      }
      return r;
    }

    static fromRotation(angle) {
      const a = num(angle, 0) * D2R;
      const c = Math.cos(a);
      const s = Math.sin(a);
      return Mat3.__fromArray([c, s, 0, -s, c, 0, 0, 0, 1]);
    }

    static fromBasis(right, up) {
      return Mat3.__fromArray([comp(right, 'x', 0), comp(right, 'y', 0), 0, comp(up, 'x', 0), comp(up, 'y', 0), 0, 0, 0, 1]);
    }

    static fromMat4(m4) {
      const a = m4.m;
      return Mat3.__fromArray([a[0], a[1], a[2], a[4], a[5], a[6], a[8], a[9], a[10]]);
    }

    static compose(translation, rotation, scale) {
      return Mat3.fromTranslation(translation).multiply(Mat3.fromRotation(rotation)).multiply(Mat3.fromScale(scale));
    }

    translation(position) {
      if (position !== undefined) {
        this.m[6] = comp(position, 'x', this.m[6]);
        this.m[7] = comp(position, 'y', this.m[7]);
      }
      return new Vec2(this.m[6], this.m[7]);
    }

    angle() {
      return Math.atan2(this.m[1], this.m[0]) * R2D;
    }

    add(other) {
      return Mat3.__fromArray(this.m.map((x, i) => x + other.m[i]));
    }

    subtract(other) {
      return Mat3.__fromArray(this.m.map((x, i) => x - other.m[i]));
    }

    multiply(value) {
      if (value instanceof Mat3) return Mat3.__fromArray(mul(this.m, value.m, 3));
      if (typeof value === 'number') return Mat3.__fromArray(this.m.map((x) => x * value));
      const v = [comp(value, 'x', 0), comp(value, 'y', 0), comp(value, 'z', 1)];
      const r = [0, 0, 0];
      for (let row = 0; row < 3; row++) {
        for (let k = 0; k < 3; k++) r[row] += this.m[k * 3 + row] * v[k];
      }
      return new Vec3(r[0], r[1], r[2]);
    }

    translate(v) {
      return this.multiply(Mat3.fromTranslation(v));
    }

    rotate(angle) {
      return this.multiply(Mat3.fromRotation(angle));
    }

    scale(v) {
      return this.multiply(Mat3.fromScale(v));
    }

    transformPoint(v) {
      const r = this.multiply(new Vec3(comp(v, 'x', 0), comp(v, 'y', 0), 1));
      return new Vec2(r.x, r.y);
    }

    transformDirection(v) {
      const r = this.multiply(new Vec3(comp(v, 'x', 0), comp(v, 'y', 0), 0));
      return new Vec2(r.x, r.y);
    }

    transpose() {
      const r = new Mat3();
      for (let c = 0; c < 3; c++) for (let row = 0; row < 3; row++) r.m[c * 3 + row] = this.m[row * 3 + c];
      return r;
    }

    determinant() {
      const a = this.m;
      return a[0] * (a[4] * a[8] - a[7] * a[5]) - a[3] * (a[1] * a[8] - a[7] * a[2]) + a[6] * (a[1] * a[5] - a[4] * a[2]);
    }

    inverse() {
      const a = this.m;
      const d = 1 / this.determinant();
      return Mat3.__fromArray([
        (a[4] * a[8] - a[5] * a[7]) * d,
        (a[2] * a[7] - a[1] * a[8]) * d,
        (a[1] * a[5] - a[2] * a[4]) * d,
        (a[5] * a[6] - a[3] * a[8]) * d,
        (a[0] * a[8] - a[2] * a[6]) * d,
        (a[2] * a[3] - a[0] * a[5]) * d,
        (a[3] * a[7] - a[4] * a[6]) * d,
        (a[1] * a[6] - a[0] * a[7]) * d,
        (a[0] * a[4] - a[1] * a[3]) * d,
      ]);
    }

    decompose() {
      const m = this.m;
      const det = m[0] * m[4] - m[3] * m[1];
      const sx = Math.hypot(m[0], m[1]) * (det < 0 ? -1 : 1);
      const sy = Math.hypot(m[3], m[4]);
      const rotation = Math.atan2(m[1] / (sx || 1), m[0] / (sx || 1)) * R2D;
      return { translation: new Vec2(m[6], m[7]), rotation, scale: new Vec2(sx, sy) };
    }

    copy() {
      return Mat3.__fromArray(this.m);
    }

    equals(other) {
      return other instanceof Mat3 && equalArrays(this.m, other.m);
    }

    toString() {
      return formatArray(this.m);
    }
  }

  return { Mat3, Mat4 };
})
