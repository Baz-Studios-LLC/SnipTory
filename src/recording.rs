//! Stream captures into a GIF with a single queued frame. Recording length never
//! determines RAM use; slow encoders drop intermediate samples, preserving time.
use anyhow::{Context, Result, bail};
use image::RgbaImage;
use std::{
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::Win32::{
    Foundation::*,
    Graphics::{Dwm::DwmFlush, Gdi::*},
    UI::WindowsAndMessaging::*,
};

#[derive(Clone)]
pub struct Control(Arc<Signals>);
struct Signals {
    mode: AtomicU8,
    end_ms: AtomicU64,
}
impl Control {
    pub fn stop(&self) {
        let _ = self
            .0
            .mode
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);
    }
    pub fn cancel(&self) {
        self.0.mode.store(2, Ordering::SeqCst);
    }
    pub fn stopping(&self) -> bool {
        self.0.mode.load(Ordering::SeqCst) != 0
    }
    pub fn cancelled(&self) -> bool {
        self.0.mode.load(Ordering::SeqCst) == 2
    }
}
pub struct RecordedGif {
    pub path: PathBuf,
    pub preview: RgbaImage,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
}
struct Sample {
    pixels: RgbaImage,
    at: Duration,
}

pub fn start(
    region: RECT,
    root: PathBuf,
    finished: impl FnOnce(Result<Option<RecordedGif>, String>) + Send + 'static,
) -> Result<Control> {
    let width = region.right - region.left;
    let height = region.bottom - region.top;
    if width <= 0
        || height <= 0
        || width > u16::MAX as i32
        || height > u16::MAX as i32
        || (width as u64 * height as u64 * 4) > 64 * 1024 * 1024
    {
        bail!("Choose a smaller GIF region (maximum 64 MiB per raw frame).");
    }
    let control = Control(Arc::new(Signals {
        mode: AtomicU8::new(0),
        end_ms: AtomicU64::new(0),
    }));
    let worker_control = control.clone();
    std::thread::Builder::new()
        .name("sniptory-gif".into())
        .spawn(move || {
            let result = run(region, root, &worker_control);
            finished(result.map_err(|e| format!("{e:#}")));
        })?;
    Ok(control)
}

fn run(region: RECT, root: PathBuf, control: &Control) -> Result<Option<RecordedGif>> {
    let folder = root.join("captures/gifs");
    fs::create_dir_all(&folder)?;
    let id = format!(
        "{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let path = folder.join(format!("{id}.gif"));
    let temporary = path.with_extension("gif.tmp");
    let (tx, rx) = mpsc::sync_channel(1);
    let capture_control = control.clone();
    let capture = std::thread::Builder::new()
        .name("sniptory-gif-capture".into())
        .spawn(move || {
            let result: Result<()> = (|| {
                let mut grabber = RegionGrabber::new(region)?;
                unsafe {
                    let _ = DwmFlush();
                }
                let started = Instant::now();
                let interval = Duration::from_nanos(1_000_000_000 / 12);
                let mut next = started;
                while !capture_control.stopping() {
                    let at = started.elapsed();
                    let sample = Sample {
                        pixels: grabber.frame()?,
                        at,
                    };
                    match tx.try_send(Ok(sample)) {
                        Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                        Err(mpsc::TrySendError::Disconnected(_)) => break,
                    }
                    next += interval;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        next = now;
                    }
                }
                capture_control
                    .0
                    .end_ms
                    .store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
                Ok(())
            })();
            if let Err(error) = result {
                let _ = tx.send(Err(format!("{error:#}")));
            }
        })?;
    let result = encode(&temporary, region, rx, control);
    if result.is_err() {
        control.cancel();
    }
    if capture.join().is_err() {
        let _ = fs::remove_file(&temporary);
        bail!("Recording capture worker stopped unexpectedly");
    }
    match result {
        Ok(Some((preview, duration_ms))) if !control.cancelled() => {
            fs::rename(&temporary, &path)?;
            Ok(Some(RecordedGif {
                path,
                preview,
                width: (region.right - region.left) as u32,
                height: (region.bottom - region.top) as u32,
                duration_ms,
            }))
        }
        Ok(_) => {
            let _ = fs::remove_file(&temporary);
            Ok(None)
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

fn encode(
    path: &std::path::Path,
    region: RECT,
    rx: mpsc::Receiver<Result<Sample, String>>,
    control: &Control,
) -> Result<Option<(RgbaImage, u64)>> {
    let first = match rx.recv() {
        Ok(Ok(first)) => first,
        Ok(Err(error)) => bail!(error),
        Err(_) if control.cancelled() => return Ok(None),
        Err(_) => bail!("Recording stopped before its first frame."),
    };
    let preview = image::imageops::thumbnail(&first.pixels, 400, 300);
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut encoder = gif::Encoder::new(
        BufWriter::new(file),
        (region.right - region.left) as u16,
        (region.bottom - region.top) as u16,
        &[],
    )?;
    encoder.set_repeat(gif::Repeat::Infinite)?;
    let mut pending = first;
    for incoming in rx {
        if control.cancelled() {
            return Ok(None);
        }
        let next = incoming.map_err(anyhow::Error::msg)?;
        write_sample(&mut encoder, &mut pending, next.at)?;
        pending = next;
    }
    if control.cancelled() {
        return Ok(None);
    }
    let duration_ms = control
        .0
        .end_ms
        .load(Ordering::SeqCst)
        .max(pending.at.as_millis() as u64 + 10);
    write_sample(
        &mut encoder,
        &mut pending,
        Duration::from_millis(duration_ms),
    )?;
    let mut writer = encoder.into_inner()?;
    writer.flush()?;
    writer.into_inner()?.sync_all()?;
    Ok(Some((preview, duration_ms)))
}

fn write_sample<W: Write>(
    encoder: &mut gif::Encoder<W>,
    sample: &mut Sample,
    end: Duration,
) -> Result<()> {
    let mut frame = gif::Frame::from_rgba_speed(
        sample.pixels.width() as u16,
        sample.pixels.height() as u16,
        sample.pixels.as_mut(),
        10,
    );
    frame.dispose = gif::DisposalMethod::Keep;
    let mut delay = ((end.as_millis() + 5) / 10)
        .saturating_sub((sample.at.as_millis() + 5) / 10)
        .max(1);
    while delay > 0 {
        frame.delay = delay.min(u16::MAX as u128) as u16;
        encoder.write_frame(&frame).context("encode GIF frame")?;
        delay -= frame.delay as u128;
    }
    Ok(())
}

struct RegionGrabber {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    region: RECT,
}
impl RegionGrabber {
    fn new(region: RECT) -> Result<Self> {
        // SAFETY: the capture thread owns this DC, selected DIB, and its memory.
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                bail!("Cannot access the desktop for recording");
            }
            let memory = CreateCompatibleDC(Some(screen));
            if memory.is_invalid() {
                ReleaseDC(None, screen);
                bail!("Cannot create recording DC");
            }
            let mut bits = std::ptr::null_mut();
            let result = CreateDIBSection(
                Some(screen),
                &crate::capture::bitmap_info(
                    region.right - region.left,
                    region.bottom - region.top,
                ),
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            );
            let bitmap = match result {
                Ok(bitmap) => bitmap,
                Err(error) => {
                    let _ = DeleteDC(memory);
                    ReleaseDC(None, screen);
                    return Err(error.into());
                }
            };
            if bits.is_null() {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(memory);
                ReleaseDC(None, screen);
                bail!("Recording buffer is unavailable");
            }
            let previous = SelectObject(memory, bitmap.into());
            Ok(Self {
                screen,
                memory,
                bitmap,
                previous,
                bits: bits.cast(),
                region,
            })
        }
    }
    fn frame(&mut self) -> Result<RgbaImage> {
        unsafe {
            let width = self.region.right - self.region.left;
            let height = self.region.bottom - self.region.top;
            BitBlt(
                self.memory,
                0,
                0,
                width,
                height,
                Some(self.screen),
                self.region.left,
                self.region.top,
                SRCCOPY | CAPTUREBLT,
            )?;
            let mut cursor = CURSORINFO {
                cbSize: std::mem::size_of::<CURSORINFO>() as u32,
                ..Default::default()
            };
            if GetCursorInfo(&mut cursor).is_ok() && cursor.flags == CURSOR_SHOWING {
                let mut icon = ICONINFO::default();
                if GetIconInfo(HICON(cursor.hCursor.0), &mut icon).is_ok() {
                    let _ = DrawIconEx(
                        self.memory,
                        cursor.ptScreenPos.x - self.region.left - icon.xHotspot as i32,
                        cursor.ptScreenPos.y - self.region.top - icon.yHotspot as i32,
                        HICON(cursor.hCursor.0),
                        0,
                        0,
                        0,
                        None,
                        DI_NORMAL,
                    );
                    if !icon.hbmMask.is_invalid() {
                        let _ = DeleteObject(icon.hbmMask.into());
                    }
                    if !icon.hbmColor.is_invalid() {
                        let _ = DeleteObject(icon.hbmColor.into());
                    }
                }
            }
            let _ = GdiFlush();
            let mut bytes =
                std::slice::from_raw_parts(self.bits, width as usize * height as usize * 4)
                    .to_vec();
            for pixel in bytes.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
            Ok(RgbaImage::from_raw(width as u32, height as u32, bytes).unwrap())
        }
    }
}
impl Drop for RegionGrabber {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.memory, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.memory);
            ReleaseDC(None, self.screen);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_gif_preserves_frames_and_elapsed_time() -> Result<()> {
        let path = std::env::temp_dir().join(format!(
            "sniptory-gif-test-{}.gif",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let control = Control(Arc::new(Signals {
            mode: AtomicU8::new(1),
            end_ms: AtomicU64::new(300),
        }));
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(Sample {
            pixels: RgbaImage::from_pixel(32, 16, image::Rgba([255, 0, 0, 255])),
            at: Duration::ZERO,
        }))?;
        tx.send(Ok(Sample {
            pixels: RgbaImage::from_pixel(32, 16, image::Rgba([0, 0, 255, 255])),
            at: Duration::from_millis(100),
        }))?;
        drop(tx);
        assert_eq!(
            encode(
                &path,
                RECT {
                    left: 0,
                    top: 0,
                    right: 32,
                    bottom: 16
                },
                rx,
                &control
            )?
            .unwrap()
            .1,
            300
        );
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(fs::File::open(&path)?)?;
        let first = decoder.read_next_frame()?.unwrap();
        assert_eq!(first.delay, 10);
        assert_eq!(&first.buffer[..4], &[255, 0, 0, 255]);
        let next = decoder.read_next_frame()?.unwrap();
        assert_eq!(next.delay, 20);
        assert_eq!(&next.buffer[..4], &[0, 0, 255, 255]);
        assert!(decoder.read_next_frame()?.is_none());
        drop(decoder);
        fs::remove_file(path)?;
        Ok(())
    }
}
