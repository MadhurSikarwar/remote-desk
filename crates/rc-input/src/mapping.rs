//! Viewer positions → host screen coordinates.

/// A rectangle in physical pixels on the virtual desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The full range of a normalised coordinate.
const FULL: i64 = 65_535;

fn scale(value: i64, numerator: i64, denominator: i64) -> i64 {
    if denominator <= 0 {
        return 0;
    }
    // Round to nearest.
    (value * numerator + denominator / 2) / denominator
}

/// The pixel on the virtual desktop that a viewer position refers to.
/// `nx`/`ny` are fractions of the shared display (0 = left/top edge,
/// 65535 = right/bottom edge).
pub fn to_pixel(nx: u16, ny: u16, display: Rect) -> (i32, i32) {
    let x = scale(i64::from(nx), i64::from(display.width) - 1, FULL);
    let y = scale(i64::from(ny), i64::from(display.height) - 1, FULL);
    (display.x + x as i32, display.y + y as i32)
}

/// A virtual-desktop pixel as the 0..=65535 "absolute" coordinates that
/// Windows expects for mouse input spanning all monitors.
pub fn to_absolute(pixel: (i32, i32), virtual_screen: Rect) -> (i32, i32) {
    let axis = |position: i32, origin: i32, size: u32| {
        let offset = i64::from(position) - i64::from(origin);
        scale(offset, FULL, i64::from(size) - 1).clamp(0, FULL) as i32
    };
    (
        axis(pixel.0, virtual_screen.x, virtual_screen.width),
        axis(pixel.1, virtual_screen.y, virtual_screen.height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FHD: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn corners_and_centre_of_a_single_display() {
        assert_eq!(to_pixel(0, 0, FHD), (0, 0));
        assert_eq!(to_pixel(65535, 65535, FHD), (1919, 1079));
        assert_eq!(to_pixel(32768, 32768, FHD), (960, 540));
    }

    #[test]
    fn positions_land_on_a_display_that_is_not_at_the_origin() {
        // A second monitor to the left of and above the primary one.
        let left = Rect {
            x: -1280,
            y: -200,
            width: 1280,
            height: 1024,
        };
        assert_eq!(to_pixel(0, 0, left), (-1280, -200));
        assert_eq!(to_pixel(65535, 65535, left), (-1, 823));
    }

    #[test]
    fn absolute_coordinates_span_the_whole_virtual_desktop() {
        let desktop = Rect {
            x: -1280,
            y: 0,
            width: 1280 + 1920,
            height: 1080,
        };
        assert_eq!(to_absolute((-1280, 0), desktop), (0, 0));
        assert_eq!(to_absolute((1919, 1079), desktop), (65535, 65535));
        // The primary display's left edge is 1280 px into a 3200 px desktop.
        assert_eq!(to_absolute((0, 0), desktop).0, 26222);
    }

    #[test]
    fn pixel_to_absolute_and_back_is_exact_for_every_column() {
        // What Windows does with an absolute coordinate: scale it back.
        for x in 0..1920 {
            let (ax, _) = to_absolute((x, 0), FHD);
            let back = (i64::from(ax) * 1919 + FULL / 2) / FULL;
            assert_eq!(back, i64::from(x));
        }
    }

    #[test]
    fn out_of_range_and_degenerate_inputs_do_not_panic() {
        assert_eq!(to_absolute((-50, 5000), FHD), (0, 65535));
        let empty = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        assert_eq!(to_pixel(65535, 65535, empty), (0, 0));
        assert_eq!(to_absolute((10, 10), empty), (0, 0));
    }
}
