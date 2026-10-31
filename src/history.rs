use anyhow::{Context, Result};
use image::RgbaImage;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub path: PathBuf,
    pub thumbnail: PathBuf,
    pub timestamp: i64,
    pub width: u32,
    pub height: u32,
    pub duration_ms: Option<u64>,
}

pub struct History {
    db: Connection,
    root: PathBuf,
}

impl History {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("captures/screenshots"))?;
        fs::create_dir_all(root.join("captures/gifs"))?;
        fs::create_dir_all(root.join("thumbnails"))?;
        let db = Connection::open(root.join("sniptory.db"))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS captures (
                id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL, kind TEXT NOT NULL,
                path TEXT NOT NULL, thumbnail TEXT NOT NULL, width INTEGER NOT NULL,
                height INTEGER NOT NULL, duration_ms INTEGER, size INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS captures_timestamp ON captures(timestamp DESC, id DESC);",
        )?;
        Ok(Self { db, root })
    }

    pub fn save(&self, pixels: &RgbaImage) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
        let id = format!("{}-{}", now.as_nanos(), std::process::id());
        let path = self
            .root
            .join("captures/screenshots")
            .join(format!("{id}.png"));
        let thumbnail = self.root.join("thumbnails").join(format!("{id}.png"));
        atomic_png(&path, pixels)?;
        let thumb = crate::thumbnail::create(pixels);
        if let Err(error) = atomic_png(&thumbnail, &thumb) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        if let Err(error) = self.db.execute(
            "INSERT INTO captures (id,timestamp,kind,path,thumbnail,width,height,size) VALUES (?1,?2,'PNG',?3,?4,?5,?6,?7)",
            params![id, now.as_secs() as i64, path.to_string_lossy(), thumbnail.to_string_lossy(), pixels.width(), pixels.height(), fs::metadata(&path)?.len() as i64],
        ) {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(&thumbnail);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn list(&self, limit: usize) -> Result<Vec<Entry>> {
        let mut statement = self.db.prepare("SELECT path,thumbnail,timestamp,width,height,duration_ms,id FROM captures ORDER BY timestamp DESC,id DESC LIMIT ?1")?;
        let entries = statement
            .query_map([limit as i64], |r| {
                Ok(Entry {
                    id: r.get(6)?,
                    path: PathBuf::from(r.get::<_, String>(0)?),
                    thumbnail: PathBuf::from(r.get::<_, String>(1)?),
                    timestamp: r.get(2)?,
                    width: r.get(3)?,
                    height: r.get(4)?,
                    duration_ms: r.get::<_, Option<i64>>(5)?.map(|value| value.max(0) as u64),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(entries)
    }

    pub fn preview(&self, entry: &Entry) -> Result<RgbaImage> {
        let expected = crate::thumbnail::dimensions(entry.width, entry.height);
        if let Ok(preview) = image::open(&entry.thumbnail) {
            if (preview.width(), preview.height()) == expected {
                return Ok(preview.to_rgba8());
            }
        }
        // Older versions forced every thumbnail to 400x300. Rebuild from the
        // untouched original; fitting an already stretched thumbnail cannot fix it.
        let pixels = image::open(&entry.path)?.to_rgba8();
        let preview = crate::thumbnail::create(&pixels);
        self.validate_file(&entry.thumbnail, true)?;
        // A cache write failure must not prevent displaying a valid capture.
        let _ = atomic_png(&entry.thumbnail, &preview);
        Ok(preview)
    }

    pub fn save_gif(&self, recording: &crate::media::RecordedGif) -> Result<()> {
        let id = recording
            .path
            .file_stem()
            .context("GIF filename is missing")?
            .to_string_lossy();
        let thumbnail = self.root.join("thumbnails").join(format!("{id}.png"));
        atomic_png(&thumbnail, &recording.preview)?;
        self.db.execute("INSERT INTO captures (id,timestamp,kind,path,thumbnail,width,height,duration_ms,size) VALUES (?1,?2,'GIF',?3,?4,?5,?6,?7,?8)",
            params![id,SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64,recording.path.to_string_lossy(),thumbnail.to_string_lossy(),recording.width,recording.height,recording.duration_ms as i64,fs::metadata(&recording.path)?.len() as i64])?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let files = self
            .db
            .query_row(
                "SELECT path,thumbnail FROM captures WHERE id=?1",
                [id],
                |row| {
                    Ok((
                        PathBuf::from(row.get::<_, String>(0)?),
                        PathBuf::from(row.get::<_, String>(1)?),
                    ))
                },
            )
            .optional()?;
        let Some((capture, thumbnail)) = files else {
            return Ok(false);
        };
        // Validate both before touching either file. A corrupted database must
        // never turn a history action into arbitrary filesystem deletion.
        self.validate_file(&capture, false)?;
        self.validate_file(&thumbnail, true)?;
        anyhow::ensure!(
            capture.file_stem().and_then(|s| s.to_str()) == Some(id)
                && thumbnail.file_stem().and_then(|s| s.to_str()) == Some(id),
            "Capture filenames do not match their history entry"
        );
        let transaction = self.db.unchecked_transaction()?;
        transaction.execute("DELETE FROM captures WHERE id=?1", [id])?;
        remove_if_present(&capture).context("Could not remove the saved capture")?;
        remove_if_present(&thumbnail).context("Could not remove the preview")?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn clear(&self) -> Result<usize> {
        // Delete only indexed captures. Leave recordings still being encoded,
        // settings, the database itself, and unrelated files alone.
        let mut statement = self.db.prepare("SELECT id FROM captures")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut removed = 0;
        for id in ids {
            if self.delete(&id).with_context(|| {
                format!("Removed {removed} capture(s); couldn't finish clearing history")
            })? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn validate_file(&self, path: &Path, thumbnail: bool) -> Result<()> {
        let root = self.root.canonicalize()?;
        let parent = path
            .parent()
            .context("Capture path has no parent")?
            .canonicalize()?;
        let allowed = if thumbnail {
            vec![self.root.join("thumbnails")]
        } else {
            vec![
                self.root.join("captures/screenshots"),
                self.root.join("captures/gifs"),
            ]
        };
        let mut trusted = false;
        for folder in allowed {
            let folder = folder.canonicalize()?;
            if folder.starts_with(&root) && parent == folder {
                trusted = true;
            }
        }
        anyhow::ensure!(
            trusted,
            "Refusing to delete a file outside SnipTory's capture folders"
        );
        match path.canonicalize() {
            Ok(resolved) => anyhow::ensure!(
                resolved.parent() == Some(parent.as_path()),
                "Refusing to follow a capture link outside its folder"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn atomic_png(path: &Path, image: &RgbaImage) -> Result<()> {
    let temporary = path.with_extension("png.tmp");
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        image
            .write_to(&mut file, image::ImageFormat::Png)
            .context("encode PNG")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_png_and_gif_previews_are_rebuilt_without_changing_originals() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "sniptory-preview-repair-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let history = History::open(root.clone())?;
        history.save(&RgbaImage::from_pixel(
            320,
            640,
            image::Rgba([10, 30, 50, 255]),
        ))?;
        let png = history.list(1)?.remove(0);
        let distorted = RgbaImage::from_pixel(400, 300, image::Rgba([100, 0, 0, 255]));
        distorted.save(&png.thumbnail)?;
        let gif_path = root.join("captures/gifs/repair.gif");
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(fs::File::create(&gif_path)?);
            encoder.encode_frame(image::Frame::new(RgbaImage::from_pixel(
                640,
                320,
                image::Rgba([20, 40, 60, 255]),
            )))?;
        }
        history.save_gif(&crate::media::RecordedGif {
            path: gif_path,
            preview: distorted,
            width: 640,
            height: 320,
            duration_ms: 100,
        })?;
        drop(history);
        let history = History::open(root.clone())?;
        for entry in history.list(60)? {
            let original = fs::read(&entry.path)?;
            let preview = history.preview(&entry)?;
            let expected = if entry.duration_ms.is_some() {
                (400, 200)
            } else {
                (150, 300)
            };
            assert_eq!(preview.dimensions(), expected);
            assert_eq!(image::open(&entry.thumbnail)?.to_rgba8(), preview);
            assert_eq!(fs::read(&entry.path)?, original);
            assert_eq!(history.preview(&entry)?, preview);
        }
        drop(history);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn captures_survive_reopening_and_have_decodable_thumbnails() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "sniptory-test-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let pixels = RgbaImage::from_pixel(640, 480, image::Rgba([37, 98, 211, 255]));
        {
            History::open(root.clone())?.save(&pixels)?;
        }
        let history = History::open(root.clone())?;
        let entries = history.list(60)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(image::open(&entries[0].path)?.to_rgba8(), pixels);
        assert_eq!(image::open(&entries[0].thumbnail)?.width(), 400);
        assert_eq!(entries[0].height, 480);
        let gif_path = root.join("captures/gifs/test.gif");
        fs::write(&gif_path, b"GIF89a")?;
        history.save_gif(&crate::media::RecordedGif {
            path: gif_path.clone(),
            preview: image::imageops::thumbnail(&pixels, 400, 300),
            width: 640,
            height: 480,
            duration_ms: 1250,
        })?;
        drop(history);
        let history = History::open(root.clone())?;
        let gif = history
            .list(60)?
            .into_iter()
            .find(|entry| entry.path == gif_path)
            .unwrap();
        assert_eq!(gif.duration_ms, Some(1250));
        assert_eq!(image::open(gif.thumbnail)?.width(), 400);
        drop(history);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn delete_and_clear_remove_managed_files_and_survive_reopening() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "sniptory-delete-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let history = History::open(root.clone())?;
        let image = RgbaImage::from_pixel(8, 8, image::Rgba([3, 4, 5, 255]));
        history.save(&image)?;
        history.save(&image)?;
        let entries = history.list(60)?;
        assert!(history.delete(&entries[0].id)?);
        assert!(!entries[0].path.exists());
        assert!(!entries[0].thumbnail.exists());
        assert!(entries[1].path.exists());
        fs::remove_file(&entries[1].path)?; // Missing originals must not block cleanup.
        let in_progress = root.join("captures/gifs/in-progress.gif.tmp");
        fs::write(&in_progress, b"unfinished")?;
        let gif = root.join("captures/gifs/gif-row.gif");
        fs::write(&gif, b"GIF89a")?;
        history.save_gif(&crate::media::RecordedGif {
            path: gif.clone(),
            preview: image.clone(),
            width: 8,
            height: 8,
            duration_ms: 100,
        })?;
        assert_eq!(history.clear()?, 2);
        assert!(!gif.exists());
        assert!(!root.join("thumbnails/gif-row.png").exists());
        assert!(in_progress.exists());
        assert!(!entries[1].thumbnail.exists());
        drop(history);
        let reopened = History::open(root.clone())?;
        assert!(reopened.list(60)?.is_empty());
        drop(reopened);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn corrupted_history_cannot_delete_outside_files() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "sniptory-delete-safety-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let history = History::open(root.clone())?;
        history.save(&RgbaImage::new(8, 8))?;
        let entry = history.list(1)?.remove(0);
        let outside = root.join("settings.json");
        fs::write(&outside, b"keep")?;
        history.db.execute(
            "UPDATE captures SET path=?1 WHERE id=?2",
            params![outside.to_string_lossy(), entry.id],
        )?;
        assert!(history.delete(&entry.id).is_err());
        assert_eq!(fs::read(&outside)?, b"keep");
        assert!(entry.thumbnail.exists());
        drop(history);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
