//! Hover previews decode one frame at a time, with at most one UI update queued.
use crate::HistoryWindow;
use image::{AnimationDecoder, ImageDecoder};
use slint::{Model, SharedPixelBuffer};
use std::{
    fs::File,
    io::BufReader,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub enum Request {
    Play {
        index: i32,
        id: String,
        path: PathBuf,
    },
    Stop,
}

pub fn start(ui: slint::Weak<HistoryWindow>) -> std::io::Result<mpsc::Sender<Request>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("sniptory-gif-preview".into())
        .spawn(move || {
            let in_flight = Arc::new(AtomicBool::new(false));
            let mut request = rx.recv().ok();
            while let Some(current) = request.take() {
                if let Request::Play { index, id, path } = current {
                    'playback: loop {
                        let Ok(file) = File::open(&path) else {
                            break;
                        };
                        let Ok(mut decoder) =
                            image::codecs::gif::GifDecoder::new(BufReader::new(file))
                        else {
                            break;
                        };
                        let (width, height) = decoder.dimensions();
                        if width as u64 * height as u64 * 4 > 64 * 1024 * 1024 {
                            break;
                        }
                        let mut limits = image::Limits::default();
                        limits.max_alloc = Some(128 * 1024 * 1024);
                        if decoder.set_limits(limits).is_err() {
                            break;
                        }
                        let mut saw_frame = false;
                        for frame in decoder.into_frames() {
                            let started = Instant::now();
                            let Ok(frame) = frame else {
                                break 'playback;
                            };
                            saw_frame = true;
                            let (numerator, denominator) = frame.delay().numer_denom_ms();
                            let delay = Duration::from_millis(
                                (numerator as u64 / denominator.max(1) as u64).max(10),
                            );
                            if !in_flight.swap(true, Ordering::SeqCst) {
                                let image = crate::thumbnail::create(frame.buffer());
                                let (w, h) = image.dimensions();
                                let bytes = image.into_raw();
                                let expected_id = id.clone();
                                let pending = in_flight.clone();
                                if ui
                                    .upgrade_in_event_loop(move |ui| {
                                        pending.store(false, Ordering::SeqCst);
                                        if ui.get_hovered() != index {
                                            return;
                                        }
                                        let model = ui.get_captures();
                                        if let Some(mut row) = model.row_data(index as usize) {
                                            if row.id.as_str() != expected_id {
                                                return;
                                            }
                                            row.preview = slint::Image::from_rgba8(
                                                SharedPixelBuffer::clone_from_slice(&bytes, w, h),
                                            );
                                            model.set_row_data(index as usize, row);
                                        }
                                    })
                                    .is_err()
                                {
                                    in_flight.store(false, Ordering::SeqCst);
                                    return;
                                }
                            }
                            match rx.recv_timeout(delay.saturating_sub(started.elapsed())) {
                                Ok(next) => {
                                    request = Some(next);
                                    break 'playback;
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                            }
                        }
                        if !saw_frame {
                            break;
                        }
                        // Reopen to loop without retaining every decoded frame.
                    }
                }
                if request.is_none() {
                    request = rx.recv().ok();
                }
            }
        })?;
    Ok(tx)
}
