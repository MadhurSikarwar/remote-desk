//! Captured frames and the row copy used to produce them.

/// One captured image, borrowed from the capturer's buffer.
///
/// Pixels are BGRA, 8 bits per channel, rows top to bottom with no padding:
/// `data.len() == width * height * 4`.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
    /// Screen updates folded into this frame since the previous one. More
    /// than 1 means the caller is reading slower than the screen changes.
    pub accumulated_updates: u32,
}

impl Frame<'_> {
    pub const BYTES_PER_PIXEL: usize = 4;

    pub fn stride(&self) -> usize {
        self.width as usize * Self::BYTES_PER_PIXEL
    }
}

/// Copies `height` rows of `row_bytes` bytes from a source whose rows are
/// `src_pitch` bytes apart into a tightly packed destination.
///
/// GPU surfaces pad each row to an alignment boundary, so `src_pitch` is often
/// larger than `row_bytes`.
///
/// # Panics
/// If `src_pitch < row_bytes` or either slice is too short.
pub fn copy_rows(src: &[u8], src_pitch: usize, dst: &mut [u8], row_bytes: usize, height: usize) {
    assert!(src_pitch >= row_bytes, "pitch smaller than a row");
    if height == 0 || row_bytes == 0 {
        return;
    }
    assert!(dst.len() >= row_bytes * height, "destination too small");
    assert!(
        src.len() >= src_pitch * (height - 1) + row_bytes,
        "source too small"
    );

    if src_pitch == row_bytes {
        dst[..row_bytes * height].copy_from_slice(&src[..row_bytes * height]);
        return;
    }
    for (src_row, dst_row) in src
        .chunks(src_pitch)
        .zip(dst.chunks_exact_mut(row_bytes))
        .take(height)
    {
        dst_row.copy_from_slice(&src_row[..row_bytes]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_tightly_packed_rows() {
        let src: Vec<u8> = (0..12).collect();
        let mut dst = [0u8; 12];
        copy_rows(&src, 4, &mut dst, 4, 3);
        assert_eq!(dst.as_slice(), src.as_slice());
    }

    #[test]
    fn drops_row_padding() {
        // 2 rows of 4 bytes, each followed by 2 bytes of padding (0xEE).
        let src = [1, 2, 3, 4, 0xEE, 0xEE, 5, 6, 7, 8, 0xEE, 0xEE];
        let mut dst = [0u8; 8];
        copy_rows(&src, 6, &mut dst, 4, 2);
        assert_eq!(dst, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn last_row_may_omit_padding() {
        let src = [1, 2, 3, 4, 0xEE, 0xEE, 5, 6, 7, 8];
        let mut dst = [0u8; 8];
        copy_rows(&src, 6, &mut dst, 4, 2);
        assert_eq!(dst, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn zero_height_copies_nothing() {
        let mut dst = [9u8; 4];
        copy_rows(&[], 4, &mut dst, 4, 0);
        assert_eq!(dst, [9; 4]);
    }

    #[test]
    #[should_panic(expected = "source too small")]
    fn rejects_short_source() {
        let mut dst = [0u8; 8];
        copy_rows(&[0u8; 7], 4, &mut dst, 4, 2);
    }

    #[test]
    #[should_panic(expected = "pitch smaller than a row")]
    fn rejects_pitch_smaller_than_row() {
        let mut dst = [0u8; 8];
        copy_rows(&[0u8; 8], 2, &mut dst, 4, 2);
    }
}
