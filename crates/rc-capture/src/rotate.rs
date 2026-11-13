//! Turning a captured surface the right way up.
//!
//! For a monitor that is rotated, Windows hands out the picture of the
//! desktop *inside an unrotated surface* (Microsoft's "Desktop Duplication
//! API", "Rotating the desktop image"): a desktop of 768 × 1024 at 90 degrees
//! arrives as a 1024 × 768 surface with the desktop turned inside it. This
//! undoes that, so what is sent is the desktop as the person at the
//! computer sees it, in the size the display reports.
//!
//! The directions are taken from Microsoft's four diagrams of the surface
//! (the numbered desktop with the start button at its bottom-left):
//!
//! | Rotation | The surface shows the desktop turned | So it is undone by turning the surface |
//! |---|---|---|
//! | 90 degrees | 90 degrees counter-clockwise (the start button at the bottom-right) | 90 degrees clockwise |
//! | 180 degrees | upside down (start button at the top-right) | 180 degrees |
//! | 270 degrees | 90 degrees clockwise (start button at the top-left) | 90 degrees counter-clockwise |

use crate::Rotation;

const PIXEL: usize = 4;

/// Writes the desktop, upright, into `dst` and returns its `(width, height)`.
/// `src` is a tightly packed BGRA surface of `width` × `height` pixels.
///
/// # Panics
/// If `src` is not exactly `width * height * 4` bytes.
pub fn upright(
    src: &[u8],
    width: usize,
    height: usize,
    rotation: Rotation,
    dst: &mut Vec<u8>,
) -> (usize, usize) {
    assert_eq!(
        src.len(),
        width * height * PIXEL,
        "the surface is not width x height"
    );
    dst.resize(src.len(), 0);
    let (out_width, out_height) = match rotation {
        Rotation::None | Rotation::Deg180 => (width, height),
        Rotation::Deg90 | Rotation::Deg270 => (height, width),
    };
    for y in 0..out_height {
        for x in 0..out_width {
            // Where this pixel of the desktop sits in the surface.
            let (sx, sy) = match rotation {
                Rotation::None => (x, y),
                Rotation::Deg180 => (width - 1 - x, height - 1 - y),
                // The surface turned 90 degrees clockwise.
                Rotation::Deg90 => (y, height - 1 - x),
                // The surface turned 90 degrees counter-clockwise.
                Rotation::Deg270 => (width - 1 - y, x),
            };
            let from = (sy * width + sx) * PIXEL;
            let to = (y * out_width + x) * PIXEL;
            dst[to..to + PIXEL].copy_from_slice(&src[from..from + PIXEL]);
        }
    }
    (out_width, out_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A surface whose four corners carry the numbers 1 to 4 (top-left,
    /// top-right, bottom-right, bottom-left) and everything else 0.
    fn corners(width: usize, height: usize, tl: u8, tr: u8, br: u8, bl: u8) -> Vec<u8> {
        let mut image = vec![0u8; width * height * PIXEL];
        for (x, y, value) in [
            (0, 0, tl),
            (width - 1, 0, tr),
            (width - 1, height - 1, br),
            (0, height - 1, bl),
        ] {
            image[(y * width + x) * PIXEL] = value;
        }
        image
    }

    /// The desktop as the person sees it: 1 at the top-left (where the
    /// window's title is), 2 top-right, 3 bottom-right, 4 bottom-left (the
    /// start button). 2 wide, 4 high.
    fn desktop() -> Vec<u8> {
        corners(2, 4, 1, 2, 3, 4)
    }

    #[test]
    fn a_90_degree_surface_is_the_desktop_turned_counter_clockwise_and_is_undone() {
        // The surface Windows delivers for a desktop at 90 degrees (Microsoft's
        // diagram): 4 wide, 2 high; the start button (4) at the bottom-right,
        // the desktop's top-right (2) at the top-left.
        let surface = corners(4, 2, 2, 3, 4, 1);
        let mut out = Vec::new();
        let size = upright(&surface, 4, 2, Rotation::Deg90, &mut out);
        assert_eq!(size, (2, 4), "a quarter turn swaps width and height");
        assert_eq!(out, desktop());
    }

    #[test]
    fn a_270_degree_surface_is_the_desktop_turned_clockwise_and_is_undone() {
        // Microsoft's diagram: the start button (4) at the top-left.
        let surface = corners(4, 2, 4, 1, 2, 3);
        let mut out = Vec::new();
        let size = upright(&surface, 4, 2, Rotation::Deg270, &mut out);
        assert_eq!(size, (2, 4));
        assert_eq!(out, desktop());
    }

    #[test]
    fn a_180_degree_surface_is_the_desktop_upside_down_and_is_undone() {
        // The start button (4) at the top-right.
        let surface = corners(2, 4, 3, 4, 1, 2);
        let mut out = Vec::new();
        let size = upright(&surface, 2, 4, Rotation::Deg180, &mut out);
        assert_eq!(size, (2, 4), "a half turn keeps the size");
        assert_eq!(out, desktop());
    }

    #[test]
    fn an_unrotated_surface_is_left_as_it_is() {
        let surface = corners(3, 2, 1, 2, 3, 4);
        let mut out = Vec::new();
        assert_eq!(upright(&surface, 3, 2, Rotation::None, &mut out), (3, 2));
        assert_eq!(out, surface);
    }

    #[test]
    fn every_pixel_goes_somewhere_and_nowhere_twice() {
        // A surface in which every pixel is different survives each rotation
        // as a permutation of itself.
        let (w, h) = (5, 3);
        let surface: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % 251) as u8, (i / 251) as u8, 7, 255])
            .collect();
        for rotation in [
            Rotation::None,
            Rotation::Deg90,
            Rotation::Deg180,
            Rotation::Deg270,
        ] {
            let mut out = Vec::new();
            let (ow, oh) = upright(&surface, w, h, rotation, &mut out);
            assert_eq!(out.len(), ow * oh * PIXEL);
            let mut before: Vec<_> = surface.chunks(PIXEL).collect();
            let mut after: Vec<_> = out.chunks(PIXEL).collect();
            before.sort_unstable();
            after.sort_unstable();
            assert_eq!(before, after, "{rotation:?} lost or repeated a pixel");
        }
    }

    #[test]
    fn the_two_quarter_turns_undo_each_other() {
        let (w, h) = (4, 3);
        let surface: Vec<u8> = (0..w * h * PIXEL).map(|i| (i % 253) as u8).collect();
        let mut turned = Vec::new();
        let (tw, th) = upright(&surface, w, h, Rotation::Deg90, &mut turned);
        let mut back = Vec::new();
        let size = upright(&turned, tw, th, Rotation::Deg270, &mut back);
        assert_eq!(size, (w, h));
        assert_eq!(back, surface);
    }
}
