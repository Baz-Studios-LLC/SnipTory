use anyhow::{Context, Result};
use image::RgbaImage;
use rusqlite::{Connection, params};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct Entry {
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
        let thumb = image::imageops::thumbnail(pixels, 400, 300);
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
        let mut statement = self.db.prepare("SELECT path,thumbnail,timestamp,width,height,duration_ms FROM captures ORDER BY timestamp DESC,id DESC LIMIT ?1")?;
        let entries = statement
            .query_map([limit as i64], |r| {
                Ok(Entry {
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

    pub fn save_gif(&self, recording: &crate::recording::RecordedGif) -> Result<()> {
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
        history.save_gif(&crate::recording::RecordedGif {
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
}
