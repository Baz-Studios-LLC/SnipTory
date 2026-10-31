//! Shared aspect-preserving previews for still captures and animated frames.
use image::RgbaImage;

pub fn dimensions(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (1, 1);
    }
    if width as u64 * 300 > height as u64 * 400 {
        let w = width.min(400);
        (w, ((height as u64 * w as u64 / width as u64) as u32).max(1))
    } else {
        let h = height.min(300);
        (((width as u64 * h as u64 / height as u64) as u32).max(1), h)
    }
}

pub fn create(pixels: &RgbaImage) -> RgbaImage {
    let (width, height) = dimensions(pixels.width(), pixels.height());
    image::imageops::thumbnail(pixels, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previews_preserve_wide_tall_square_and_small_images() {
        for (source, expected) in [
            ((1600, 900), (400, 225)),
            ((900, 1600), (168, 300)),
            ((800, 800), (300, 300)),
            ((80, 20), (80, 20)),
            ((1, 10000), (1, 300)),
        ] {
            let image = RgbaImage::from_pixel(source.0, source.1, image::Rgba([23, 45, 67, 255]));
            let preview = create(&image);
            assert_eq!(preview.dimensions(), expected);
            assert_eq!(preview.get_pixel(0, 0), image.get_pixel(0, 0));
        }
    }
}
