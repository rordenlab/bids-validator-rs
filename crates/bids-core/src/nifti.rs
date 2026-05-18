//! NIfTI-1 / NIfTI-2 header parser.
//!
//! BIDS validation only needs the header fields (`dim`, `pixdim`,
//! `xyzt_units`, `intent_code`, `qform_code`, `sform_code`) — never the
//! voxel data. The header is the first 348 bytes for NIfTI-1 (`magic
//! = "n+1"` or `"ni1"`) or 540 bytes for NIfTI-2 (`magic = "n+2"`).
//!
//! `.nii` files are read directly; `.nii.gz` files are inflated only
//! until 540 bytes of output are produced, then the stream is dropped.
//!
//! Mirrors the partial-read tactic described in `plan.md` Phase 0a.

use std::io::{Cursor, Read};

use flate2::read::GzDecoder;

/// Parsed NIfTI header. Only the fields BIDS rules actually reference
/// are exposed; voxel data offset / scaling / orientation matrices are
/// out of scope.
#[derive(Debug, Clone)]
pub struct NiftiHeader {
    /// `dim[0..=7]`. `dim[0]` is the number of dimensions in use (1–7);
    /// `dim[1..=dim[0]]` are the actual extents.
    pub dim: [i64; 8],
    /// `pixdim[0..=7]`. `pixdim[0]` is the qfac sign; `pixdim[i]` is
    /// the voxel size along dim `i`. Float for NIfTI-1, double for NIfTI-2.
    pub pixdim: [f64; 8],
    /// Combined spatial + temporal units. Low 3 bits = spatial,
    /// next 3 bits = temporal. See `nifti1.h`.
    pub xyzt_units: u8,
    pub intent_code: i16,
    pub qform_code: i16,
    pub sform_code: i16,
    pub axis_codes: Option<Vec<String>>,
}

/// Read enough from `reader` to parse the NIfTI header. Handles both
/// uncompressed NIfTI and gzip-compressed NIfTI by sniffing the first
/// two bytes. Returns `None` if the stream isn't a NIfTI header (wrong
/// magic, too short, etc.). Caller is responsible for emitting
/// `NIFTI_HEADER_UNREADABLE` if desired.
pub fn read_header<R: Read>(reader: R) -> Option<NiftiHeader> {
    let bytes = read_first_bytes(reader, 540).ok()?;
    parse_header(&bytes)
}

fn read_first_bytes<R: Read>(mut reader: R, n: usize) -> std::io::Result<Vec<u8>> {
    // `.nii.gz` heuristic: check the gzip magic on the on-disk bytes.
    // Avoid extension-based dispatch because the BIDS validator must
    // accept files even when the on-disk name doesn't reflect
    // compression cleanly.
    let mut head = [0u8; 2];
    let n_peeked = read_exact_up_to(&mut reader, &mut head)?;
    let mut buf = vec![0u8; n];
    if n_peeked >= 2 && head[0] == 0x1f && head[1] == 0x8b {
        let prefix = Cursor::new(head[..n_peeked].to_vec());
        let mut decoder = GzDecoder::new(prefix.chain(reader));
        let read = read_exact_up_to(&mut decoder, &mut buf)?;
        buf.truncate(read);
        Ok(buf)
    } else {
        let initial = n_peeked.min(n);
        buf[..initial].copy_from_slice(&head[..initial]);
        let rest = if initial < n {
            read_exact_up_to(&mut reader, &mut buf[initial..])?
        } else {
            0
        };
        buf.truncate(initial + rest);
        Ok(buf)
    }
}

fn read_exact_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn parse_header(bytes: &[u8]) -> Option<NiftiHeader> {
    // Detect NIfTI-1 vs NIfTI-2 by the `sizeof_hdr` field at offset 0.
    // NIfTI-1: 348 in native or swapped form. NIfTI-2: 540.
    if bytes.len() < 4 {
        return None;
    }
    let sizeof_hdr_native = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i32;
    let sizeof_hdr_be = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i32;
    let (size, little_endian) = if sizeof_hdr_native == 348 {
        (348, true)
    } else if sizeof_hdr_be == 348 {
        (348, false)
    } else if sizeof_hdr_native == 540 {
        (540, true)
    } else if sizeof_hdr_be == 540 {
        (540, false)
    } else {
        return None;
    };

    // Magic-byte check. Pre-fix any file whose first 4 bytes happened
    // to be 348 / 540 (a 4-byte integer) was treated as a NIfTI
    // header. Random binary data with that 32-bit prefix would
    // populate `nifti_header` and trigger NIfTI-derived rules. The
    // magic is the disambiguator. Audited as High #10 (2026-05-17).
    //
    // NIfTI-1 magic at offset 344: `"ni1\0"` (separate .img/.hdr) or
    //   `"n+1\0"` (single .nii / .nii.gz).
    // NIfTI-2 magic at offset 4 (right after sizeof_hdr): an 8-byte
    //   prefix `"ni2\0\r\n\x1a\n"` or `"n+2\0\r\n\x1a\n"`. The four
    //   trailing bytes detect line-ending corruption introduced by FTP
    //   or other text-mode transfers.
    let magic_ok = match size {
        348 => bytes.len() >= 348 && matches!(&bytes[344..348], b"ni1\0" | b"n+1\0"),
        540 => {
            bytes.len() >= 12 && matches!(&bytes[4..12], b"ni2\0\r\n\x1a\n" | b"n+2\0\r\n\x1a\n")
        }
        _ => false,
    };
    if !magic_ok {
        return None;
    }

    match size {
        348 => parse_nifti1(bytes, little_endian),
        540 => parse_nifti2(bytes, little_endian),
        _ => None,
    }
}

fn parse_nifti1(bytes: &[u8], le: bool) -> Option<NiftiHeader> {
    if bytes.len() < 348 {
        return None;
    }
    // dim[8] at offset 40, i16 each. (Offsets per nifti1.h.)
    let mut dim = [0i64; 8];
    for (i, slot) in dim.iter_mut().enumerate() {
        let off = 40 + i * 2;
        let raw = if le {
            i16::from_le_bytes([bytes[off], bytes[off + 1]])
        } else {
            i16::from_be_bytes([bytes[off], bytes[off + 1]])
        };
        *slot = raw as i64;
    }
    // pixdim[8] at offset 76, f32 each.
    let mut pixdim = [0f64; 8];
    for (i, slot) in pixdim.iter_mut().enumerate() {
        let off = 76 + i * 4;
        *slot = read_f32(&bytes[off..off + 4], le) as f64;
    }
    // intent_code at 68 (i16), xyzt_units at 123 (u8), qform_code at 252 (i16),
    // sform_code at 254 (i16).
    let intent_code = read_i16(&bytes[68..70], le);
    let xyzt_units = bytes[123];
    let qform_code = read_i16(&bytes[252..254], le);
    let sform_code = read_i16(&bytes[254..256], le);
    let axis_codes = nifti1_axis_codes(bytes, le, qform_code, sform_code, pixdim);
    Some(NiftiHeader {
        dim,
        pixdim,
        xyzt_units,
        intent_code,
        qform_code,
        sform_code,
        axis_codes,
    })
}

fn parse_nifti2(bytes: &[u8], le: bool) -> Option<NiftiHeader> {
    if bytes.len() < 540 {
        return None;
    }
    // NIfTI-2 (nifti2.h): dim[8] at offset 16, i64 each. pixdim[8] at
    // offset 104, f64 each. intent_code at 504, xyzt_units at 500,
    // qform_code at 344, sform_code at 348.
    let mut dim = [0i64; 8];
    for (i, slot) in dim.iter_mut().enumerate() {
        let off = 16 + i * 8;
        *slot = read_i64(&bytes[off..off + 8], le);
    }
    let mut pixdim = [0f64; 8];
    for (i, slot) in pixdim.iter_mut().enumerate() {
        let off = 104 + i * 8;
        *slot = read_f64(&bytes[off..off + 8], le);
    }
    // NIfTI-2 layout: intent_code is i32 at offset 504, xyzt_units is i32 at 500,
    // qform_code i32 at 344, sform_code i32 at 348. We narrow to i16 for parity
    // with the NIfTI-1 path (BIDS schema doesn't care about precision here).
    let intent_code = read_i32(&bytes[504..508], le) as i16;
    let xyzt_units = read_i32(&bytes[500..504], le) as u8;
    let qform_code = read_i32(&bytes[344..348], le) as i16;
    let sform_code = read_i32(&bytes[348..352], le) as i16;
    let axis_codes = nifti2_axis_codes(bytes, le, qform_code, sform_code, pixdim);
    Some(NiftiHeader {
        dim,
        pixdim,
        xyzt_units,
        intent_code,
        qform_code,
        sform_code,
        axis_codes,
    })
}

fn nifti1_axis_codes(
    bytes: &[u8],
    le: bool,
    qform_code: i16,
    sform_code: i16,
    pixdim: [f64; 8],
) -> Option<Vec<String>> {
    if sform_code > 0 {
        return axis_codes_from_affine(sform_affine_f32(bytes, le, 280));
    }
    if qform_code > 0 {
        let b = read_f32(&bytes[256..260], le) as f64;
        let c = read_f32(&bytes[260..264], le) as f64;
        let d = read_f32(&bytes[264..268], le) as f64;
        let x = read_f32(&bytes[268..272], le) as f64;
        let y = read_f32(&bytes[272..276], le) as f64;
        let z = read_f32(&bytes[276..280], le) as f64;
        return axis_codes_from_affine(qform_affine(b, c, d, x, y, z, pixdim));
    }
    None
}

fn nifti2_axis_codes(
    bytes: &[u8],
    le: bool,
    qform_code: i16,
    sform_code: i16,
    pixdim: [f64; 8],
) -> Option<Vec<String>> {
    if sform_code > 0 {
        return axis_codes_from_affine(sform_affine_f64(bytes, le, 400));
    }
    if qform_code > 0 {
        let b = read_f64(&bytes[352..360], le);
        let c = read_f64(&bytes[360..368], le);
        let d = read_f64(&bytes[368..376], le);
        let x = read_f64(&bytes[376..384], le);
        let y = read_f64(&bytes[384..392], le);
        let z = read_f64(&bytes[392..400], le);
        return axis_codes_from_affine(qform_affine(b, c, d, x, y, z, pixdim));
    }
    None
}

fn sform_affine_f32(bytes: &[u8], le: bool, offset: usize) -> [[f64; 4]; 4] {
    let mut affine = [[0.0; 4]; 4];
    for (row, out_row) in affine.iter_mut().enumerate().take(3) {
        for (col, out) in out_row.iter_mut().enumerate() {
            let off = offset + row * 16 + col * 4;
            *out = read_f32(&bytes[off..off + 4], le) as f64;
        }
    }
    affine[3][3] = 1.0;
    affine
}

fn sform_affine_f64(bytes: &[u8], le: bool, offset: usize) -> [[f64; 4]; 4] {
    let mut affine = [[0.0; 4]; 4];
    for (row, out_row) in affine.iter_mut().enumerate().take(3) {
        for (col, out) in out_row.iter_mut().enumerate() {
            let off = offset + row * 32 + col * 8;
            *out = read_f64(&bytes[off..off + 8], le);
        }
    }
    affine[3][3] = 1.0;
    affine
}

fn qform_affine(
    b: f64,
    c: f64,
    d: f64,
    qx: f64,
    qy: f64,
    qz: f64,
    pixdim: [f64; 8],
) -> [[f64; 4]; 4] {
    let mut a_sq = 1.0 - (b * b + c * c + d * d);
    if a_sq < 0.0 && a_sq > -1e-7 {
        a_sq = 0.0;
    }
    let a = a_sq.sqrt();
    let dx = if pixdim[1] == 0.0 { 1.0 } else { pixdim[1] };
    let dy = if pixdim[2] == 0.0 { 1.0 } else { pixdim[2] };
    let qfac = if pixdim[0] < 0.0 { -1.0 } else { 1.0 };
    let dz = (if pixdim[3] == 0.0 { 1.0 } else { pixdim[3] }) * qfac;

    let r11 = a * a + b * b - c * c - d * d;
    let r12 = 2.0 * b * c - 2.0 * a * d;
    let r13 = 2.0 * b * d + 2.0 * a * c;
    let r21 = 2.0 * b * c + 2.0 * a * d;
    let r22 = a * a + c * c - b * b - d * d;
    let r23 = 2.0 * c * d - 2.0 * a * b;
    let r31 = 2.0 * b * d - 2.0 * a * c;
    let r32 = 2.0 * c * d + 2.0 * a * b;
    let r33 = a * a + d * d - c * c - b * b;

    [
        [r11 * dx, r12 * dy, r13 * dz, qx],
        [r21 * dx, r22 * dy, r23 * dz, qy],
        [r31 * dx, r32 * dy, r33 * dz, qz],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn axis_codes_from_affine(affine: [[f64; 4]; 4]) -> Option<Vec<String>> {
    if affine.iter().flatten().any(|v| !v.is_finite()) {
        return None;
    }

    let cos_x = [affine[0][0], affine[1][0], affine[2][0]];
    let cos_y = [affine[0][1], affine[1][1], affine[2][1]];
    let cos_z = [affine[0][2], affine[1][2], affine[2][2]];

    let orth_x = norm3(cos_x)?;
    let orth_y = norm3(sub3(cos_y, scale3(orth_x, dot3(orth_x, cos_y))))?;
    let orth_z = norm3(sub3(
        cos_z,
        add3(
            scale3(orth_x, dot3(orth_x, cos_z)),
            scale3(orth_y, dot3(orth_y, cos_z)),
        ),
    ))?;

    let basis = [orth_x, orth_y, orth_z];
    let mut magnitudes = basis.map(|row| row.map(f64::abs));
    let mut dims = [0usize, 1, 2];
    dims.sort_by(|a, b| {
        let ma = max3(magnitudes[*a]);
        let mb = max3(magnitudes[*b]);
        mb.partial_cmp(&ma).unwrap_or(std::cmp::Ordering::Equal)
    });

    let codes = [["R", "L"], ["A", "P"], ["S", "I"]];
    let mut result = ["", "", ""];
    for dim in dims {
        let idx = argmax3(magnitudes[dim]);
        for row in &mut magnitudes {
            row[idx] = 0.0;
        }
        result[dim] = codes[idx][usize::from(basis[dim][idx] <= 0.0)];
    }

    Some(result.iter().map(|s| (*s).to_string()).collect())
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm3(v: [f64; 3]) -> Option<[f64; 3]> {
    let norm = dot3(v, v).sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return None;
    }
    Some(scale3(v, 1.0 / norm))
}

fn scale3(v: [f64; 3], scalar: f64) -> [f64; 3] {
    v.map(|x| x * scalar)
}

fn add3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn max3(a: [f64; 3]) -> f64 {
    a[0].max(a[1]).max(a[2])
}

fn argmax3(a: [f64; 3]) -> usize {
    if a[1] > a[0] && a[1] >= a[2] {
        1
    } else if a[2] > a[0] && a[2] > a[1] {
        2
    } else {
        0
    }
}

// These five `read_*` helpers take a `&[u8]` and panic on a short
// slice. That is *intentional* — every caller in this file is reached
// from `parse_nifti1` (guarded by `bytes.len() < 348`) or `parse_nifti2`
// (guarded by `bytes.len() < 540`), so the slice is by construction long
// enough. We use `chunk::<N>` (a const-generic) to turn that contract
// into a single point of panic per width, with a clear message — so any
// future refactor that lets these be reached from a shorter buffer
// fails loudly here instead of crashing several lines downstream.
fn chunk<const N: usize>(b: &[u8]) -> [u8; N] {
    <[u8; N]>::try_from(&b[..N]).expect(
        "nifti read helper called with a slice shorter than the requested width; \
         caller must have already length-checked the buffer (see parse_nifti1/2)",
    )
}

fn read_i16(b: &[u8], le: bool) -> i16 {
    let arr = chunk::<2>(b);
    if le {
        i16::from_le_bytes(arr)
    } else {
        i16::from_be_bytes(arr)
    }
}

fn read_i32(b: &[u8], le: bool) -> i32 {
    let arr = chunk::<4>(b);
    if le {
        i32::from_le_bytes(arr)
    } else {
        i32::from_be_bytes(arr)
    }
}

fn read_i64(b: &[u8], le: bool) -> i64 {
    let arr = chunk::<8>(b);
    if le {
        i64::from_le_bytes(arr)
    } else {
        i64::from_be_bytes(arr)
    }
}

fn read_f32(b: &[u8], le: bool) -> f32 {
    let arr = chunk::<4>(b);
    if le {
        f32::from_le_bytes(arr)
    } else {
        f32::from_be_bytes(arr)
    }
}

fn read_f64(b: &[u8], le: bool) -> f64 {
    let arr = chunk::<8>(b);
    if le {
        f64::from_le_bytes(arr)
    } else {
        f64::from_be_bytes(arr)
    }
}

/// Convert a parsed header to the `Value::Object` the selector DSL
/// expects. Mirrors the `nifti_header` shape exposed by the TS
/// validator (`src/files/nifti.ts`).
///
/// In addition to the raw `dim`/`pixdim` arrays, this exposes:
///   * `shape` — `dim[1..=dim[0]]`, the meaningful per-axis extents.
///   * `voxel_sizes` — `pixdim[1..=dim[0]]`, voxel/sample sizes.
///   * `xyzt_units` as a structured `{xyz, t}` object (TS shape:
///     `xyzt_units` becomes a nested object after parsing).
pub fn header_to_json(h: &NiftiHeader) -> serde_json::Value {
    let ndim = h.dim[0].clamp(0, 7) as usize;
    let shape: Vec<i64> = (1..=ndim).map(|i| h.dim[i]).collect();
    let voxel_sizes: Vec<f64> = (1..=ndim).map(|i| h.pixdim[i]).collect();
    // xyzt_units: low 2 bits = spatial-unit index, bits 3-4 = temporal
    // -unit index. Mirrors TS `src/files/nifti.ts:65-67`.
    //   xyz: ['unknown', 'meter', 'mm', 'um'][xyzt_units & 0x03]
    //   t:   ['unknown', 'sec',   'msec','usec'][(xyzt_units >> 3) & 0x03]
    let xyz_idx = (h.xyzt_units & 0x03) as usize;
    let t_idx = ((h.xyzt_units >> 3) & 0x03) as usize;
    let xyz = ["unknown", "meter", "mm", "um"][xyz_idx.min(3)];
    let t = ["unknown", "sec", "msec", "usec"][t_idx.min(3)];
    serde_json::json!({
        "dim": h.dim,
        "pixdim": h.pixdim,
        "shape": shape,
        "voxel_sizes": voxel_sizes,
        "xyzt_units": {"xyz": xyz, "t": t},
        "intent_code": h.intent_code,
        "qform_code": h.qform_code,
        "sform_code": h.sform_code,
        "axis_codes": h.axis_codes.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::{Cursor, Write};

    /// Build a minimal little-endian NIfTI-1 header in memory.
    fn nifti1_le(dim: [i16; 8], pixdim: [f32; 8], xyzt_units: u8) -> Vec<u8> {
        let mut buf = vec![0u8; 352]; // 348 hdr + 4-byte vox_offset slack
                                      // sizeof_hdr = 348
        buf[0..4].copy_from_slice(&348i32.to_le_bytes());
        // dim[8] at offset 40
        for (i, d) in dim.iter().enumerate() {
            buf[40 + i * 2..40 + i * 2 + 2].copy_from_slice(&d.to_le_bytes());
        }
        // pixdim[8] at offset 76
        for (i, p) in pixdim.iter().enumerate() {
            buf[76 + i * 4..76 + i * 4 + 4].copy_from_slice(&p.to_le_bytes());
        }
        buf[123] = xyzt_units;
        // magic at 344 ("n+1\0")
        buf[344..348].copy_from_slice(b"n+1\0");
        buf
    }

    #[test]
    fn parses_nifti1_little_endian() {
        let bytes = nifti1_le(
            [3, 64, 64, 36, 1, 1, 1, 1],
            [1.0, 2.5, 2.5, 3.0, 0.0, 0.0, 0.0, 0.0],
            10, // mm + sec → 2 (mm) | 8 (sec) = 10
        );
        let h = parse_header(&bytes).expect("header parses");
        assert_eq!(h.dim, [3, 64, 64, 36, 1, 1, 1, 1]);
        assert!((h.pixdim[1] - 2.5).abs() < 1e-6);
        assert_eq!(h.xyzt_units, 10);
    }

    #[test]
    fn reads_uncompressed_header_from_reader() {
        let bytes = nifti1_le(
            [3, 64, 64, 36, 1, 1, 1, 1],
            [1.0, 2.5, 2.5, 3.0, 0.0, 0.0, 0.0, 0.0],
            10,
        );
        let h = read_header(Cursor::new(bytes)).expect("header parses");
        assert_eq!(h.dim, [3, 64, 64, 36, 1, 1, 1, 1]);
    }

    #[test]
    fn reads_gzipped_header_from_reader() {
        let bytes = nifti1_le(
            [3, 64, 64, 36, 1, 1, 1, 1],
            [1.0, 2.5, 2.5, 3.0, 0.0, 0.0, 0.0, 0.0],
            10,
        );
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let compressed = encoder.finish().unwrap();

        let h = read_header(Cursor::new(compressed)).expect("header parses");
        assert_eq!(h.dim, [3, 64, 64, 36, 1, 1, 1, 1]);
    }

    #[test]
    fn axis_codes_match_identity_and_flips() {
        let ras = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let spl = [
            [0.0, 0.0, -1.0, 0.0],
            [0.0, -1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(
            axis_codes_from_affine(ras).unwrap(),
            vec!["R".to_string(), "A".to_string(), "S".to_string()]
        );
        assert_eq!(
            axis_codes_from_affine(spl).unwrap(),
            vec!["S".to_string(), "P".to_string(), "L".to_string()]
        );
    }

    /// Regression for audit High #10. Pre-fix, any 348-byte file whose
    /// first 4 bytes happened to decode to 348 (little-endian) was
    /// treated as a NIfTI-1 header and populated `nifti_header`,
    /// triggering NIfTI-derived rules against random binary data.
    #[test]
    fn rejects_non_nifti_348_byte_data() {
        let mut buf = vec![0u8; 348];
        buf[0..4].copy_from_slice(&348i32.to_le_bytes());
        // No magic bytes at 344..348 — pre-fix this populated a header
        // anyway. Post-fix: rejected.
        assert!(parse_header(&buf).is_none());
    }

    #[test]
    fn rejects_wrong_magic_at_344() {
        let mut bytes = nifti1_le(
            [3, 64, 64, 36, 1, 1, 1, 1],
            [1.0, 2.5, 2.5, 3.0, 0.0, 0.0, 0.0, 0.0],
            10,
        );
        bytes[344..348].copy_from_slice(b"BOGUS"[0..4].try_into().unwrap());
        assert!(parse_header(&bytes).is_none());
    }
}
