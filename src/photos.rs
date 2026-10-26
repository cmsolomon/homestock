use crate::error::AppError;
use image::{
    codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage, ImageDecoder, ImageReader,
};
use sha2::{Digest, Sha256};
use std::{io::Cursor, path::PathBuf};

/// Decode → apply EXIF orientation → fit within 1600 px → JPEG q82 (drops EXIF/GPS)
/// → save as `<sha256>.jpg`. Returns the public path `photos/<hash>.jpg`.
pub async fn store(dir: PathBuf, bytes: Vec<u8>) -> Result<String, AppError> {
    tokio::task::spawn_blocking(move || -> Result<String, AppError> {
        let mut dec = ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()?
            .into_decoder()
            .map_err(|e| AppError::bad(format!("Unsupported image: {e}")))?;
        let orient = dec
            .orientation()
            .map_err(|e| AppError::bad(e.to_string()))?;
        let mut img = DynamicImage::from_decoder(dec).map_err(|e| AppError::bad(e.to_string()))?;
        img.apply_orientation(orient);
        if img.width() > 1600 || img.height() > 1600 {
            img = img.resize(1600, 1600, FilterType::Lanczos3);
        }

        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 82)
            .encode_image(&img.to_rgb8())
            .map_err(|e| AppError::internal(e.to_string()))?;
        let hash = hex::encode(Sha256::digest(&out));
        let file = dir.join(format!("{hash}.jpg"));
        if !file.exists() {
            let tmp = dir.join(format!("{hash}.jpg.part"));
            std::fs::write(&tmp, &out)?;
            std::fs::rename(&tmp, &file)?;
        }
        Ok(format!("photos/{hash}.jpg"))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}
