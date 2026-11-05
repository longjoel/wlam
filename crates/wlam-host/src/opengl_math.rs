//! Column-major OpenGL fixed-function matrix helpers.
//!
//! Matrices use column vectors and are stored as `m[column * 4 + row]`, as in
//! OpenGL. Transform helpers post-multiply (`current * transform`), matching
//! `glTranslate`, `glRotate`, `glOrtho` and the GLU projection/view helpers.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MatrixError {
    #[error("matrix argument is not finite")]
    NonFinite,
    #[error("matrix bounds are degenerate or outside the finite range")]
    InvalidBounds,
    #[error("perspective field of view must be between 0 and 180 degrees")]
    InvalidFieldOfView,
    #[error("perspective aspect ratio must be positive")]
    InvalidAspect,
    #[error("perspective near and far planes must satisfy 0 < near < far")]
    InvalidClipPlanes,
    #[error("rotation axis must be nonzero")]
    ZeroAxis,
    #[error("look-at eye and center must differ")]
    CoincidentEyeAndCenter,
    #[error("look-at up vector must not be parallel to the view direction")]
    ParallelUp,
}

/// A 4×4 column-major matrix. Entry `(row, column)` is at `column * 4 + row`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [f32; 16]);

impl Default for Mat4 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat4 {
    pub const IDENTITY: Self = Self([
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]);

    pub const fn identity() -> Self {
        Self::IDENTITY
    }

    pub fn get(self, row: usize, column: usize) -> f32 {
        self.0[column * 4 + row]
    }

    /// Return `self * rhs`, the post-multiplication used by legacy OpenGL.
    pub fn multiply(self, rhs: Self) -> Self {
        let mut out = [0.0; 16];
        for column in 0..4 {
            for row in 0..4 {
                out[column * 4 + row] = (0..4).map(|k| self.get(row, k) * rhs.get(k, column)).sum();
            }
        }
        Self(out)
    }

    pub fn translated(self, x: f32, y: f32, z: f32) -> Result<Self, MatrixError> {
        finite(&[x, y, z])?;
        let mut t = Self::IDENTITY;
        t.0[12] = x;
        t.0[13] = y;
        t.0[14] = z;
        Ok(self.multiply(t))
    }

    /// Post-multiply a right-handed rotation. `angle_degrees` follows OpenGL's
    /// API convention; `axis` is normalized before constructing the matrix.
    pub fn rotated(self, angle_degrees: f32, axis: [f32; 3]) -> Result<Self, MatrixError> {
        finite(&[angle_degrees, axis[0], axis[1], axis[2]])?;
        let [x, y, z] = normalize(axis).ok_or(MatrixError::ZeroAxis)?;
        let radians = angle_degrees.to_radians();
        let (s, c) = radians.sin_cos();
        let one_c = 1.0 - c;
        let rotation = Self([
            x * x * one_c + c,
            y * x * one_c + z * s,
            z * x * one_c - y * s,
            0.0,
            x * y * one_c - z * s,
            y * y * one_c + c,
            z * y * one_c + x * s,
            0.0,
            x * z * one_c + y * s,
            y * z * one_c - x * s,
            z * z * one_c + c,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ]);
        Ok(self.multiply(rotation))
    }

    /// Post-multiply the OpenGL orthographic projection matrix.
    pub fn ortho(
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        near: f32,
        far: f32,
    ) -> Result<Self, MatrixError> {
        finite(&[left, right, bottom, top, near, far])?;
        if left == right || bottom == top || near == far {
            return Err(MatrixError::InvalidBounds);
        }
        let (dx, dy, dz) = (right - left, top - bottom, far - near);
        finite(&[dx, dy, dz])?;
        Ok(Self([
            2.0 / dx,
            0.0,
            0.0,
            0.0,
            0.0,
            2.0 / dy,
            0.0,
            0.0,
            0.0,
            0.0,
            -2.0 / dz,
            0.0,
            -(right + left) / dx,
            -(top + bottom) / dy,
            -(far + near) / dz,
            1.0,
        ]))
    }

    /// Build the perspective matrix used by `gluPerspective`, with the field
    /// of view expressed in degrees and the conventional OpenGL clip range.
    pub fn perspective(
        fovy_degrees: f32,
        aspect: f32,
        near: f32,
        far: f32,
    ) -> Result<Self, MatrixError> {
        finite(&[fovy_degrees, aspect, near, far])?;
        if !(0.0 < fovy_degrees && fovy_degrees < 180.0) {
            return Err(MatrixError::InvalidFieldOfView);
        }
        if aspect <= 0.0 {
            return Err(MatrixError::InvalidAspect);
        }
        if near <= 0.0 || far <= near {
            return Err(MatrixError::InvalidClipPlanes);
        }
        let f = 1.0 / (0.5 * fovy_degrees.to_radians()).tan();
        let dz = far - near;
        Ok(Self([
            f / aspect,
            0.0,
            0.0,
            0.0,
            0.0,
            f,
            0.0,
            0.0,
            0.0,
            0.0,
            -(far + near) / dz,
            -1.0,
            0.0,
            0.0,
            -(2.0 * far * near) / dz,
            0.0,
        ]))
    }

    /// Build the GLU perspective matrix from its double-precision API inputs,
    /// rounding only when the result enters the renderer's f32 matrix state.
    pub fn perspective_f64(
        fovy_degrees: f64,
        aspect: f64,
        near: f64,
        far: f64,
    ) -> Result<Self, MatrixError> {
        if ![fovy_degrees, aspect, near, far]
            .into_iter()
            .all(f64::is_finite)
        {
            return Err(MatrixError::NonFinite);
        }
        if !(0.0 < fovy_degrees && fovy_degrees < 180.0) {
            return Err(MatrixError::InvalidFieldOfView);
        }
        if aspect <= 0.0 {
            return Err(MatrixError::InvalidAspect);
        }
        if near <= 0.0 || far <= near {
            return Err(MatrixError::InvalidClipPlanes);
        }
        // Match the reference GLU sequence: radians = fovy / 2 * pi / 180,
        // then cotangent = cos(radians) / sin(radians).
        let radians = fovy_degrees / 2.0 * std::f64::consts::PI / 180.0;
        let sine = radians.sin();
        if sine == 0.0 {
            return Err(MatrixError::InvalidFieldOfView);
        }
        let cotangent = radians.cos() / sine;
        let dz = far - near;
        Ok(Self([
            (cotangent / aspect) as f32,
            0.0,
            0.0,
            0.0,
            0.0,
            cotangent as f32,
            0.0,
            0.0,
            0.0,
            0.0,
            (-(far + near) / dz) as f32,
            -1.0,
            0.0,
            0.0,
            (-2.0 * near * far / dz) as f32,
            0.0,
        ]))
    }

    /// Build the right-handed view matrix used by `gluLookAt`.
    pub fn look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> Result<Self, MatrixError> {
        finite(&[
            eye[0], eye[1], eye[2], center[0], center[1], center[2], up[0], up[1], up[2],
        ])?;
        let forward = normalize(sub(center, eye)).ok_or(MatrixError::CoincidentEyeAndCenter)?;
        let side = normalize(cross(forward, up)).ok_or(MatrixError::ParallelUp)?;
        let corrected_up = cross(side, forward);
        let view = Self([
            side[0],
            corrected_up[0],
            -forward[0],
            0.0,
            side[1],
            corrected_up[1],
            -forward[1],
            0.0,
            side[2],
            corrected_up[2],
            -forward[2],
            0.0,
            -dot(side, eye),
            -dot(corrected_up, eye),
            dot(forward, eye),
            1.0,
        ]);
        Ok(view)
    }
}

fn finite(values: &[f32]) -> Result<(), MatrixError> {
    if values.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        Err(MatrixError::NonFinite)
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let length = dot(v, v).sqrt();
    if !length.is_finite() || length == 0.0 {
        None
    } else {
        Some([v[0] / length, v[1] / length, v[2] / length])
    }
}
