use image::RgbaImage;
use std::path::PathBuf;

/// A completed recording, independent of the OS that supplied its frames.
pub struct RecordedGif {
    pub path: PathBuf,
    pub preview: RgbaImage,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
}
