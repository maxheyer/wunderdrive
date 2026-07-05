//! ocrs-backed OCR engine (spec §6: "text-layer first, OCR only as fallback").
//!
//! This module implements [`extract::OcrEngine`] using the pure-Rust [`ocrs`]
//! crate running on the [`rten`] ML inference engine. Models are loaded from
//! `.rten` files whose paths are configured via environment variables:
//!
//! - `WUNDERDRIVE_OCR_DETECTION_MODEL` — path to `text-detection.rten`
//! - `WUNDERDRIVE_OCR_RECOGNITION_MODEL` — path to `text-recognition.rten`
//!
//! If either env var is unset or the model file cannot be loaded, OCR is
//! silently disabled — the engine returns `None` and the indexer falls back
//! to "no text" for that file. This is graceful degradation per the spec:
//! OCR is a fallback, not a requirement.
//!
//! Models are downloaded by the user (they're ~10 MB total). See
//! <https://github.com/robertknight/ocrs> for download instructions.

use std::path::PathBuf;

use ocrs::{ImageSource, OcrEngine as OcrsInner, OcrEngineParams};
use tracing::{debug, warn};

use crate::extract::OcrEngine;

/// Environment variable for the text-detection model path.
pub const ENV_DETECTION_MODEL: &str = "WUNDERDRIVE_OCR_DETECTION_MODEL";
/// Environment variable for the text-recognition model path.
pub const ENV_RECOGNITION_MODEL: &str = "WUNDERDRIVE_OCR_RECOGNITION_MODEL";

/// ocrs-backed implementation of [`OcrEngine`].
///
/// Holds the initialized ocrs engine (which owns the loaded rten models).
/// Construction loads models from disk; if that fails the instance wraps
/// `None` and [`ocr`](OcrEngine::ocr) always returns `None`.
pub struct OcrsEngine {
    engine: Option<OcrsInner>,
}

impl OcrsEngine {
    /// Construct from explicit model file paths.
    ///
    /// Returns an `OcrsEngine` with OCR disabled (wraps `None`) if either
    /// path is `None`, if the file doesn't exist, or if the model fails to
    /// load. Never returns `Err` — OCR is best-effort.
    pub fn new(
        detection_model_path: Option<&std::path::Path>,
        recognition_model_path: Option<&std::path::Path>,
    ) -> Self {
        let engine = Self::try_build(detection_model_path, recognition_model_path);
        if engine.is_none() {
            warn!("OCR disabled: models not loaded");
        } else {
            debug!("OCR enabled: ocrs engine initialized");
        }
        OcrsEngine { engine }
    }

    /// Construct from environment variables
    /// (`WUNDERDRIVE_OCR_DETECTION_MODEL`, `WUNDERDRIVE_OCR_RECOGNITION_MODEL`).
    ///
    /// If both vars are set and point to readable `.rten` files, OCR is
    /// enabled. Otherwise it's disabled gracefully.
    pub fn from_env() -> Self {
        let det = std::env::var(ENV_DETECTION_MODEL).ok().map(PathBuf::from);
        let rec = std::env::var(ENV_RECOGNITION_MODEL).ok().map(PathBuf::from);
        Self::new(det.as_deref(), rec.as_deref())
    }

    /// Returns `true` if models are loaded and OCR is active.
    pub fn is_enabled(&self) -> bool {
        self.engine.is_some()
    }

    /// Attempt to load both models and build the ocrs engine.
    /// Returns `None` on any failure (missing file, parse error, etc.).
    fn try_build(
        detection_model_path: Option<&std::path::Path>,
        recognition_model_path: Option<&std::path::Path>,
    ) -> Option<OcrsInner> {
        let det_path = detection_model_path?;
        let rec_path = recognition_model_path?;

        let detection_model = Self::load_model(det_path, "detection")?;
        let recognition_model = Self::load_model(rec_path, "recognition")?;

        let engine = OcrsInner::new(OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            ..Default::default()
        })
        .map_err(|e| warn!(error = %e, "failed to create ocrs engine"))
        .ok()?;

        Some(engine)
    }

    /// Load a single `.rten` model from a file path. Returns `None` and logs
    /// on failure (file missing, corrupt model, etc.).
    fn load_model(path: &std::path::Path, kind: &str) -> Option<rten::Model> {
        if !path.exists() {
            warn!(kind = kind, path = %path.display(), "OCR model file not found");
            return None;
        }
        rten::Model::load_file(path)
            .map_err(|e| warn!(kind = kind, path = %path.display(), error = %e, "failed to load OCR model"))
            .ok()
    }
}

impl OcrEngine for OcrsEngine {
    fn ocr(&self, image_bytes: &[u8]) -> Option<String> {
        let engine = self.engine.as_ref()?;

        // Decode image bytes → RGB8. The `image` crate auto-detects format
        // from the magic bytes (PNG, JPEG, WebP, BMP, GIF, etc.).
        let img = image::load_from_memory(image_bytes)
            .map_err(|e| debug!(error = %e, "failed to decode image for OCR"))
            .ok()?
            .into_rgb8();

        // Feed to ocrs via ImageSource (HWC byte tensor, channels=3).
        let img_source = ImageSource::from_bytes(img.as_raw(), img.dimensions())
            .map_err(|e| debug!(error = %e, "failed to create ImageSource for OCR"))
            .ok()?;

        // Prepare preprocessed input (greyscale, normalized to [-0.5, 0.5]).
        let ocr_input = engine
            .prepare_input(img_source)
            .map_err(|e| debug!(error = %e, "ocrs prepare_input failed"))
            .ok()?;

        // Detect + layout-analyze + recognize → single text string.
        let text = engine
            .get_text(&ocr_input)
            .map_err(|e| debug!(error = %e, "ocrs get_text failed"))
            .ok()?;

        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
}
