//! BGRA → NV12 colour conversion.
//!
//! NV12 is what video encoders take: a full-resolution luma plane followed by
//! one half-resolution plane of interleaved Cb/Cr. Conversion uses the BT.709
//! matrix with limited ("video") range, the convention for HD video.

/// Bytes needed for an NV12 image.
pub fn nv12_len(width: usize, height: usize) -> usize {
    width * height + 2 * (width / 2) * (height / 2)
}

/// The largest even size not exceeding `value`. H.264 4:2:0 frames need even
/// dimensions, so an odd screen loses its last row or column.
pub fn even(value: u32) -> u32 {
    value & !1
}

#[inline]
fn luma(b: u32, g: u32, r: u32) -> u8 {
    (16 + ((47 * r + 157 * g + 16 * b + 128) >> 8)) as u8
}

#[inline]
fn chroma(b: i32, g: i32, r: i32) -> (u8, u8) {
    let cb = 128 + ((-26 * r - 86 * g + 112 * b + 128) >> 8);
    let cr = 128 + ((112 * r - 102 * g - 10 * b + 128) >> 8);
    (cb.clamp(16, 240) as u8, cr.clamp(16, 240) as u8)
}

/// Converts the top-left `width` × `height` pixels of a BGRA image whose rows
/// are `src_stride` bytes apart into `dst` (NV12, tightly packed).
///
/// # Panics
/// If `width` or `height` is odd, or a buffer is too small.
pub fn bgra_to_nv12(src: &[u8], src_stride: usize, width: usize, height: usize, dst: &mut [u8]) {
    assert!(
        width.is_multiple_of(2) && height.is_multiple_of(2),
        "dimensions must be even"
    );
    assert!(src_stride >= width * 4, "stride smaller than a row");
    if width == 0 || height == 0 {
        return;
    }
    assert!(
        src.len() >= src_stride * (height - 1) + width * 4,
        "source too small"
    );
    assert!(
        dst.len() >= nv12_len(width, height),
        "destination too small"
    );

    let (y_plane, uv_plane) = dst.split_at_mut(width * height);

    for (row_pair, (y_rows, uv_row)) in y_plane
        .chunks_exact_mut(width * 2)
        .zip(uv_plane.chunks_exact_mut(width))
        .enumerate()
    {
        let top = &src[row_pair * 2 * src_stride..][..width * 4];
        let bottom = &src[(row_pair * 2 + 1) * src_stride..][..width * 4];
        let (y_top, y_bottom) = y_rows.split_at_mut(width);

        // Each step takes a 2×2 block: two BGRA pixels from each row.
        let (top_blocks, _) = top.as_chunks::<8>();
        let (bottom_blocks, _) = bottom.as_chunks::<8>();
        let (uv_pairs, _) = uv_row.as_chunks_mut::<2>();

        for (x, ((top_block, bottom_block), uv)) in top_blocks
            .iter()
            .zip(bottom_blocks)
            .zip(uv_pairs)
            .enumerate()
        {
            let mut sum = [0u32; 3];
            for i in 0..2 {
                for (block, y_row) in [(top_block, &mut *y_top), (bottom_block, &mut *y_bottom)] {
                    let b = u32::from(block[i * 4]);
                    let g = u32::from(block[i * 4 + 1]);
                    let r = u32::from(block[i * 4 + 2]);
                    y_row[x * 2 + i] = luma(b, g, r);
                    sum = [sum[0] + b, sum[1] + g, sum[2] + r];
                }
            }
            // Chroma is the average colour of the block.
            let avg = |v: u32| ((v + 2) / 4) as i32;
            let (cb, cr) = chroma(avg(sum[0]), avg(sum[1]), avg(sum[2]));
            *uv = [cb, cr];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: usize, height: usize, bgra: [u8; 4]) -> Vec<u8> {
        bgra.iter()
            .copied()
            .cycle()
            .take(width * height * 4)
            .collect()
    }

    fn convert(src: &[u8], width: usize, height: usize) -> Vec<u8> {
        let mut dst = vec![0xAA; nv12_len(width, height)];
        bgra_to_nv12(src, width * 4, width, height, &mut dst);
        dst
    }

    fn assert_solid(bgra: [u8; 4], y: u8, cb: u8, cr: u8) {
        let out = convert(&solid(4, 2, bgra), 4, 2);
        assert_eq!(&out[..8], &[y; 8], "luma for {bgra:?}");
        assert_eq!(&out[8..], &[cb, cr, cb, cr], "chroma for {bgra:?}");
    }

    #[test]
    fn sizes() {
        assert_eq!(nv12_len(1920, 1080), 1920 * 1080 * 3 / 2);
        assert_eq!(nv12_len(4, 2), 12);
        assert_eq!((even(1367), even(768), even(1), even(0)), (1366, 768, 0, 0));
    }

    #[test]
    fn black_white_and_grey_have_neutral_chroma() {
        assert_solid([0, 0, 0, 255], 16, 128, 128);
        assert_solid([255, 255, 255, 255], 235, 128, 128);
        assert_solid([128, 128, 128, 255], 126, 128, 128);
    }

    #[test]
    fn primaries_match_bt709_limited_range() {
        // Reference values: Y = 16 + 219·(0.2126R + 0.7152G + 0.0722B),
        // Cb/Cr = 128 + 224·(…), rounded; integer maths may differ by 1.
        let near = |bgra: [u8; 4], y: i32, cb: i32, cr: i32| {
            let out = convert(&solid(2, 2, bgra), 2, 2);
            let got = (i32::from(out[0]), i32::from(out[4]), i32::from(out[5]));
            assert!(
                (got.0 - y).abs() <= 1 && (got.1 - cb).abs() <= 1 && (got.2 - cr).abs() <= 1,
                "{bgra:?}: got {got:?}, expected ({y}, {cb}, {cr})"
            );
        };
        near([0, 0, 255, 255], 63, 102, 240); // red
        near([0, 255, 0, 255], 173, 42, 26); // green
        near([255, 0, 0, 255], 32, 240, 118); // blue
    }

    #[test]
    fn chroma_is_averaged_over_each_block_and_luma_is_per_pixel() {
        // One 2×2 block: black, white / white, black.
        let k = [0, 0, 0, 255];
        let w = [255, 255, 255, 255];
        let src: Vec<u8> = [k, w, w, k].concat();
        let out = convert(&src, 2, 2);
        assert_eq!(&out[..4], &[16, 235, 235, 16]);
        assert_eq!(&out[4..], &[128, 128]);
    }

    #[test]
    fn honours_stride_and_ignores_alpha() {
        // 2×2 red with alpha 0 and 4 bytes of padding per row.
        let row = [0, 0, 255, 0, 0, 0, 255, 0, 9, 9, 9, 9];
        let src: Vec<u8> = [row, row].concat();
        let mut dst = vec![0; nv12_len(2, 2)];
        bgra_to_nv12(&src, 12, 2, 2, &mut dst);
        assert_eq!(dst, convert(&solid(2, 2, [0, 0, 255, 255]), 2, 2));
    }

    #[test]
    #[should_panic(expected = "dimensions must be even")]
    fn rejects_odd_sizes() {
        let mut dst = vec![0; 16];
        bgra_to_nv12(&[0; 48], 12, 3, 2, &mut dst);
    }
}
