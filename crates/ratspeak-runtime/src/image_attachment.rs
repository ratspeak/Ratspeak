//! Bounded, metadata-free preparation of still-image LXMF attachments.
//!
//! The WebView is intentionally limited to selecting a profile and displaying
//! a tiny prepared preview. Source inspection and image transformation operate
//! on private staging files so a large photo never crosses IPC as one value.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::imageops::FilterType as ResizeFilter;
use image::metadata::Orientation;
use image::{DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const IMAGE_SIZE_PROMPT_BYTES: usize = 1_000_000;
pub const IMAGE_MAX_PIXELS: u64 = 16_000_000;
pub const IMAGE_MAX_EDGE: u32 = 8_192;
pub const IMAGE_PREVIEW_MAX_EDGE: u32 = 192;
const IMAGE_DECODE_MAX_ALLOC_BYTES: u64 = 192 * 1024 * 1024;
const IMAGE_PREVIEW_MAX_BYTES: usize = 256 * 1024;
const MAX_ENCODE_ATTEMPTS: usize = 6;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSizeProfile {
    Small,
    Medium,
    Large,
    Actual,
}

impl ImageSizeProfile {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
            Self::Actual => "Actual size",
        }
    }

    pub const fn max_edge(self) -> Option<u32> {
        match self {
            Self::Small => Some(960),
            Self::Medium => Some(1_600),
            Self::Large => Some(2_560),
            Self::Actual => None,
        }
    }

    pub const fn byte_ceiling(self) -> Option<usize> {
        match self {
            Self::Small => Some(250_000),
            Self::Medium => Some(750_000),
            Self::Large => Some(2_000_000),
            Self::Actual => None,
        }
    }

    fn jpeg_qualities(self) -> &'static [u8] {
        match self {
            Self::Small => &[72, 64, 56, 48, 42, 38],
            Self::Medium => &[82, 76, 70, 64, 58, 52],
            Self::Large => &[90, 86, 82, 78, 72, 66],
            Self::Actual => &[95],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageAttachmentDisposition {
    Still,
    Animated,
    Unsupported,
    TooLarge,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImageSizeOption {
    pub profile: ImageSizeProfile,
    pub label: &'static str,
    pub recommended: bool,
    pub max_edge: Option<u32>,
    pub byte_ceiling: Option<usize>,
    pub estimated_bytes: usize,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImageAttachmentInspection {
    pub disposition: ImageAttachmentDisposition,
    pub source_bytes: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub mime: Option<&'static str>,
    pub format: Option<&'static str>,
    pub should_prompt: bool,
    pub options: Vec<ImageSizeOption>,
}

#[derive(Debug)]
pub struct PreparedImageAttachment {
    pub path: PathBuf,
    pub file_name: String,
    pub mime: &'static str,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub profile: ImageSizeProfile,
    pub preview_mime: &'static str,
    pub preview_bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum ImageAttachmentError {
    #[error("Image staging is unavailable")]
    Io(#[from] std::io::Error),
    #[error("Image format is unsupported")]
    Unsupported,
    #[error("Animated images must be sent as files")]
    Animated,
    #[error("Image is too large to process safely")]
    TooLarge,
    #[error("Image could not be decoded or encoded")]
    Codec(#[from] image::ImageError),
    #[error("Image could not meet the selected size")]
    CannotMeetProfile,
    #[error("Prepared image exceeds the supported attachment limit")]
    OutputTooLarge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputFormat {
    Jpeg,
    Png,
}

impl OutputFormat {
    const fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }
}

pub fn inspect_image_attachment(
    path: &Path,
) -> Result<ImageAttachmentInspection, ImageAttachmentError> {
    let source_bytes = usize::try_from(std::fs::metadata(path)?.len())
        .map_err(|_| ImageAttachmentError::TooLarge)?;
    let (format, reader) = open_image_reader(path)?;
    let mut decoder = reader.into_decoder()?;
    let (raw_width, raw_height) = decoder.dimensions();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let (width, height) = oriented_dimensions(raw_width, raw_height, orientation);
    let (format_name, mime) = format_identity(format).ok_or(ImageAttachmentError::Unsupported)?;
    let animated = format_is_animated(path, format)?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(ImageAttachmentError::TooLarge)?;
    let within_processing_limits = width > 0
        && height > 0
        && width <= IMAGE_MAX_EDGE
        && height <= IMAGE_MAX_EDGE
        && pixels <= IMAGE_MAX_PIXELS;
    let disposition = if animated {
        ImageAttachmentDisposition::Animated
    } else if !within_processing_limits {
        ImageAttachmentDisposition::TooLarge
    } else {
        ImageAttachmentDisposition::Still
    };
    let options = if disposition == ImageAttachmentDisposition::Still {
        image_size_options(source_bytes, width, height, format)
    } else {
        Vec::new()
    };
    Ok(ImageAttachmentInspection {
        disposition,
        source_bytes,
        width: Some(width),
        height: Some(height),
        mime: Some(mime),
        format: Some(format_name),
        should_prompt: image_size_should_prompt(source_bytes),
        options,
    })
}

pub const fn image_size_should_prompt(source_bytes: usize) -> bool {
    source_bytes > IMAGE_SIZE_PROMPT_BYTES
}

pub fn unsupported_image_inspection(source_bytes: usize) -> ImageAttachmentInspection {
    unavailable_image_inspection(source_bytes, ImageAttachmentDisposition::Unsupported)
}

pub fn unavailable_image_inspection(
    source_bytes: usize,
    disposition: ImageAttachmentDisposition,
) -> ImageAttachmentInspection {
    ImageAttachmentInspection {
        disposition,
        source_bytes,
        width: None,
        height: None,
        mime: None,
        format: None,
        should_prompt: false,
        options: Vec::new(),
    }
}

pub fn image_size_options(
    source_bytes: usize,
    width: u32,
    height: u32,
    format: ImageFormat,
) -> Vec<ImageSizeOption> {
    [
        ImageSizeProfile::Small,
        ImageSizeProfile::Medium,
        ImageSizeProfile::Large,
        ImageSizeProfile::Actual,
    ]
    .into_iter()
    .map(|profile| ImageSizeOption {
        profile,
        label: profile.label(),
        recommended: profile == ImageSizeProfile::Medium,
        max_edge: profile.max_edge(),
        byte_ceiling: profile.byte_ceiling(),
        estimated_bytes: estimate_profile_bytes(source_bytes, width, height, format, profile),
        available: true,
        unavailable_reason: None,
    })
    .collect()
}

pub fn estimate_profile_bytes(
    source_bytes: usize,
    width: u32,
    height: u32,
    format: ImageFormat,
    profile: ImageSizeProfile,
) -> usize {
    if profile == ImageSizeProfile::Actual || width == 0 || height == 0 {
        return source_bytes;
    }
    let (target_width, target_height) = profile_dimensions(width, height, profile);
    let source_pixels = f64::from(width) * f64::from(height);
    let target_pixels = f64::from(target_width) * f64::from(target_height);
    let pixel_ratio = (target_pixels / source_pixels).clamp(0.0, 1.0);
    let codec_factor = match format {
        ImageFormat::Bmp => 0.22,
        ImageFormat::Png => 0.82,
        ImageFormat::Gif => 0.72,
        ImageFormat::WebP => 0.86,
        _ => 0.90,
    };
    let profile_factor = match profile {
        ImageSizeProfile::Small => 0.72,
        ImageSizeProfile::Medium => 0.84,
        ImageSizeProfile::Large => 0.94,
        ImageSizeProfile::Actual => 1.0,
    };
    let calculated = ((source_bytes as f64) * pixel_ratio * codec_factor * profile_factor)
        .round()
        .max(4_000.0) as usize;
    calculated
        .min(source_bytes)
        .min(profile.byte_ceiling().unwrap_or(usize::MAX))
}

pub fn prepare_image_attachment(
    source_path: &Path,
    output_path: &Path,
    source_name: &str,
    profile: ImageSizeProfile,
    max_output_bytes: usize,
) -> Result<PreparedImageAttachment, ImageAttachmentError> {
    let inspection = inspect_image_attachment(source_path)?;
    match inspection.disposition {
        ImageAttachmentDisposition::Still => {}
        ImageAttachmentDisposition::Animated => return Err(ImageAttachmentError::Animated),
        ImageAttachmentDisposition::TooLarge => return Err(ImageAttachmentError::TooLarge),
        ImageAttachmentDisposition::Unsupported => return Err(ImageAttachmentError::Unsupported),
    }
    let format =
        image_format_from_name(inspection.format.ok_or(ImageAttachmentError::Unsupported)?)
            .ok_or(ImageAttachmentError::Unsupported)?;
    let (format_again, reader) = open_image_reader(source_path)?;
    if format_again != format {
        return Err(ImageAttachmentError::Unsupported);
    }
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let source_has_alpha = decoder.color_type().has_alpha();
    let decoded = DynamicImage::from_decoder(decoder)?;
    let mut original = if source_has_alpha {
        DynamicImage::ImageRgba8(decoded.into_rgba8())
    } else {
        DynamicImage::ImageRgb8(decoded.into_rgb8())
    };
    original.apply_orientation(orientation);

    let output_format = output_format_for(format, source_has_alpha);
    let mut dimensions = profile_dimensions(original.width(), original.height(), profile);
    let qualities = profile.jpeg_qualities();
    let byte_ceiling = profile
        .byte_ceiling()
        .unwrap_or(max_output_bytes)
        .min(max_output_bytes);
    let mut prepared = None;

    for attempt in 0..MAX_ENCODE_ATTEMPTS {
        let candidate = if dimensions == original.dimensions() {
            None
        } else {
            Some(original.resize(dimensions.0, dimensions.1, ResizeFilter::Lanczos3))
        };
        let image = candidate.as_ref().unwrap_or(&original);
        let quality = qualities[attempt.min(qualities.len() - 1)];
        encode_image_to_path(image, output_path, output_format, quality)?;
        let encoded_size = usize::try_from(std::fs::metadata(output_path)?.len())
            .map_err(|_| ImageAttachmentError::OutputTooLarge)?;
        if encoded_size <= byte_ceiling {
            let preview = encode_preview(image)?;
            prepared = Some((image.dimensions(), encoded_size, preview));
            break;
        }
        if profile == ImageSizeProfile::Actual {
            return Err(ImageAttachmentError::OutputTooLarge);
        }
        let scale = ((byte_ceiling as f64 / encoded_size as f64).sqrt() * 0.92).clamp(0.35, 0.88);
        let next_width = ((f64::from(dimensions.0) * scale).floor() as u32).max(64);
        let next_height = ((f64::from(dimensions.1) * scale).floor() as u32).max(64);
        if (next_width, next_height) == dimensions {
            break;
        }
        dimensions = (next_width, next_height);
    }

    let ((width, height), size, (preview_mime, preview_bytes)) =
        prepared.ok_or(ImageAttachmentError::CannotMeetProfile)?;
    let file_name = prepared_file_name(source_name, output_format);
    Ok(PreparedImageAttachment {
        path: output_path.to_path_buf(),
        file_name,
        mime: output_format.mime(),
        size,
        width,
        height,
        profile,
        preview_mime,
        preview_bytes,
    })
}

fn open_image_reader(
    path: &Path,
) -> Result<(ImageFormat, ImageReader<BufReader<File>>), ImageAttachmentError> {
    let file = File::open(path)?;
    let mut reader = ImageReader::new(BufReader::new(file)).with_guessed_format()?;
    let format = reader.format().ok_or(ImageAttachmentError::Unsupported)?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(IMAGE_DECODE_MAX_ALLOC_BYTES);
    reader.limits(limits);
    Ok((format, reader))
}

fn oriented_dimensions(width: u32, height: u32, orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    }
}

fn profile_dimensions(width: u32, height: u32, profile: ImageSizeProfile) -> (u32, u32) {
    let Some(max_edge) = profile.max_edge() else {
        return (width, height);
    };
    let longest = width.max(height);
    if longest <= max_edge {
        return (width, height);
    }
    let scale = f64::from(max_edge) / f64::from(longest);
    (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    )
}

fn output_format_for(format: ImageFormat, source_has_alpha: bool) -> OutputFormat {
    if source_has_alpha || matches!(format, ImageFormat::Gif) {
        OutputFormat::Png
    } else {
        OutputFormat::Jpeg
    }
}

fn encode_image_to_path(
    image: &DynamicImage,
    path: &Path,
    format: OutputFormat,
    jpeg_quality: u8,
) -> Result<(), ImageAttachmentError> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let mut writer = BufWriter::new(file);
    match format {
        OutputFormat::Jpeg => {
            let encoder = JpegEncoder::new_with_quality(&mut writer, jpeg_quality);
            image.write_with_encoder(encoder)?;
        }
        OutputFormat::Png => {
            let encoder = PngEncoder::new_with_quality(
                &mut writer,
                CompressionType::Best,
                FilterType::Adaptive,
            );
            image.write_with_encoder(encoder)?;
        }
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn encode_preview(image: &DynamicImage) -> Result<(&'static str, Vec<u8>), ImageAttachmentError> {
    let preview = image.thumbnail(IMAGE_PREVIEW_MAX_EDGE, IMAGE_PREVIEW_MAX_EDGE);
    let mut bytes = Vec::new();
    let mime = if preview.color().has_alpha() {
        let encoder =
            PngEncoder::new_with_quality(&mut bytes, CompressionType::Fast, FilterType::Adaptive);
        preview.write_with_encoder(encoder)?;
        "image/png"
    } else {
        let encoder = JpegEncoder::new_with_quality(&mut bytes, 76);
        preview.write_with_encoder(encoder)?;
        "image/jpeg"
    };
    if bytes.len() > IMAGE_PREVIEW_MAX_BYTES {
        bytes.clear();
    }
    Ok((mime, bytes))
}

fn prepared_file_name(source_name: &str, format: OutputFormat) -> String {
    let stem = Path::new(source_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("image");
    format!("{stem}.{}", format.extension())
}

fn format_identity(format: ImageFormat) -> Option<(&'static str, &'static str)> {
    match format {
        ImageFormat::Jpeg => Some(("jpeg", "image/jpeg")),
        ImageFormat::Png => Some(("png", "image/png")),
        ImageFormat::Gif => Some(("gif", "image/gif")),
        ImageFormat::WebP => Some(("webp", "image/webp")),
        ImageFormat::Bmp => Some(("bmp", "image/bmp")),
        _ => None,
    }
}

fn image_format_from_name(name: &str) -> Option<ImageFormat> {
    match name {
        "jpeg" => Some(ImageFormat::Jpeg),
        "png" => Some(ImageFormat::Png),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::WebP),
        "bmp" => Some(ImageFormat::Bmp),
        _ => None,
    }
}

fn format_is_animated(path: &Path, format: ImageFormat) -> Result<bool, ImageAttachmentError> {
    match format {
        ImageFormat::Gif => gif_has_multiple_frames(path).map_err(map_image_structure_io),
        ImageFormat::Png => png_has_animation(path).map_err(map_image_structure_io),
        ImageFormat::WebP => webp_has_animation(path).map_err(map_image_structure_io),
        _ => Ok(false),
    }
}

fn map_image_structure_io(error: std::io::Error) -> ImageAttachmentError {
    match error.kind() {
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::InvalidData => {
            ImageAttachmentError::Unsupported
        }
        _ => ImageAttachmentError::Io(error),
    }
}

fn gif_has_multiple_frames(path: &Path) -> std::io::Result<bool> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut header = [0u8; 13];
    reader.read_exact(&mut header)?;
    if !matches!(&header[..6], b"GIF87a" | b"GIF89a") {
        return Ok(false);
    }
    if header[10] & 0x80 != 0 {
        let table_bytes = 3u64 << (u32::from(header[10] & 0x07) + 1);
        reader.seek(SeekFrom::Current(
            i64::try_from(table_bytes).unwrap_or(i64::MAX),
        ))?;
    }
    let mut frames = 0usize;
    loop {
        let mut marker = [0u8; 1];
        if reader.read_exact(&mut marker).is_err() {
            return Ok(false);
        }
        match marker[0] {
            0x2c => {
                frames += 1;
                if frames > 1 {
                    return Ok(true);
                }
                let mut descriptor = [0u8; 9];
                reader.read_exact(&mut descriptor)?;
                if descriptor[8] & 0x80 != 0 {
                    let table_bytes = 3u64 << (u32::from(descriptor[8] & 0x07) + 1);
                    reader.seek(SeekFrom::Current(
                        i64::try_from(table_bytes).unwrap_or(i64::MAX),
                    ))?;
                }
                reader.seek(SeekFrom::Current(1))?;
                skip_gif_sub_blocks(&mut reader)?;
            }
            0x21 => {
                reader.seek(SeekFrom::Current(1))?;
                skip_gif_sub_blocks(&mut reader)?;
            }
            0x3b => return Ok(false),
            _ => return Ok(false),
        }
    }
}

fn skip_gif_sub_blocks(reader: &mut BufReader<File>) -> std::io::Result<()> {
    loop {
        let mut size = [0u8; 1];
        reader.read_exact(&mut size)?;
        if size[0] == 0 {
            return Ok(());
        }
        reader.seek(SeekFrom::Current(i64::from(size[0])))?;
    }
}

fn png_has_animation(path: &Path) -> std::io::Result<bool> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut signature = [0u8; 8];
    reader.read_exact(&mut signature)?;
    if signature != *b"\x89PNG\r\n\x1a\n" {
        return Ok(false);
    }
    loop {
        let mut header = [0u8; 8];
        reader.read_exact(&mut header)?;
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let kind = &header[4..8];
        if kind == b"acTL" {
            return Ok(true);
        }
        if kind == b"IDAT" || kind == b"IEND" {
            return Ok(false);
        }
        reader.seek(SeekFrom::Current(i64::from(length) + 4))?;
    }
}

fn webp_has_animation(path: &Path) -> std::io::Result<bool> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut header = [0u8; 12];
    reader.read_exact(&mut header)?;
    if &header[..4] != b"RIFF" || &header[8..12] != b"WEBP" {
        return Ok(false);
    }
    loop {
        let mut chunk = [0u8; 8];
        if reader.read_exact(&mut chunk).is_err() {
            return Ok(false);
        }
        let length = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        if &chunk[..4] == b"ANIM" || &chunk[..4] == b"ANMF" {
            return Ok(true);
        }
        if &chunk[..4] == b"VP8X" && length >= 1 {
            let mut flags = [0u8; 1];
            reader.read_exact(&mut flags)?;
            if flags[0] & 0x02 != 0 {
                return Ok(true);
            }
            reader.seek(SeekFrom::Current(
                i64::from(length - 1) + i64::from(length & 1),
            ))?;
        } else {
            reader.seek(SeekFrom::Current(i64::from(length) + i64::from(length & 1)))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, ImageBuffer, Rgb, Rgba};
    use tempfile::TempDir;

    fn fixture_png(path: &Path, width: u32, height: u32, alpha: u8) {
        let image = DynamicImage::ImageRgba8(ImageBuffer::from_fn(width, height, |x, y| {
            Rgba([
                ((x * 17 + y * 3) % 255) as u8,
                ((x * 7 + y * 13) % 255) as u8,
                ((x * 5 + y * 19) % 255) as u8,
                alpha,
            ])
        }));
        encode_image_to_path(&image, path, OutputFormat::Png, 90).unwrap();
    }

    fn fixture_jpeg(path: &Path, width: u32, height: u32) {
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([
                ((x * 17 + y * 3) % 255) as u8,
                ((x * 7 + y * 13) % 255) as u8,
                ((x * 5 + y * 19) % 255) as u8,
            ])
        }));
        encode_image_to_path(&image, path, OutputFormat::Jpeg, 92).unwrap();
    }

    fn add_exif_orientation(path: &Path, orientation: u16) {
        let source = std::fs::read(path).unwrap();
        assert_eq!(&source[..2], &[0xff, 0xd8]);
        let mut app1 = vec![0xff, 0xe1, 0x00, 0x22];
        app1.extend_from_slice(b"Exif\0\0II*\0\x08\0\0\0\x01\0");
        app1.extend_from_slice(&0x0112u16.to_le_bytes());
        app1.extend_from_slice(&3u16.to_le_bytes());
        app1.extend_from_slice(&1u32.to_le_bytes());
        app1.extend_from_slice(&orientation.to_le_bytes());
        app1.extend_from_slice(&[0, 0]);
        app1.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(app1.len(), 36);
        let mut oriented = source[..2].to_vec();
        oriented.extend_from_slice(&app1);
        oriented.extend_from_slice(&source[2..]);
        std::fs::write(path, oriented).unwrap();
    }

    #[test]
    fn estimates_are_bounded_and_actual_tracks_source() {
        let options = image_size_options(6_000_000, 4_000, 3_000, ImageFormat::Jpeg);
        assert_eq!(options.len(), 4);
        assert!(options[0].estimated_bytes <= 250_000);
        assert!(options[1].estimated_bytes <= 750_000);
        assert!(options[2].estimated_bytes <= 2_000_000);
        assert_eq!(options[3].estimated_bytes, 6_000_000);
        assert!(options[1].recommended);
    }

    #[test]
    fn prompt_uses_exact_decimal_megabyte_boundary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("photo.png");
        fixture_png(&path, 20, 20, 255);
        let inspection = inspect_image_attachment(&path).unwrap();
        assert!(!inspection.should_prompt);
        assert_eq!(IMAGE_SIZE_PROMPT_BYTES, 1_000_000);
        assert!(!image_size_should_prompt(1_000_000));
        assert!(image_size_should_prompt(1_000_001));
    }

    #[test]
    fn selected_profile_is_the_only_output_and_preserves_alpha() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.png");
        let output = dir.path().join("prepared");
        fixture_png(&source, 1_200, 900, 120);
        let prepared = prepare_image_attachment(
            &source,
            &output,
            "field-photo.png",
            ImageSizeProfile::Small,
            128_000_000,
        )
        .unwrap();
        assert_eq!(prepared.mime, "image/png");
        assert!(prepared.width <= 960);
        assert!(prepared.height <= 960);
        assert!(prepared.size <= 250_000);
        assert!(!prepared.preview_bytes.is_empty());
        assert!(prepared.preview_bytes.len() <= IMAGE_PREVIEW_MAX_BYTES);
        let decoded = ImageReader::with_format(
            BufReader::new(File::open(&output).unwrap()),
            ImageFormat::Png,
        )
        .decode()
        .unwrap();
        assert!(decoded.color().has_alpha());
    }

    #[test]
    fn opaque_jpeg_remains_jpeg_and_meets_medium_ceiling() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.jpg");
        let output = dir.path().join("prepared");
        fixture_jpeg(&source, 2_400, 1_600);
        let prepared = prepare_image_attachment(
            &source,
            &output,
            "trail.jpg",
            ImageSizeProfile::Medium,
            128_000_000,
        )
        .unwrap();
        assert_eq!(prepared.mime, "image/jpeg");
        assert_eq!(prepared.file_name, "trail.jpg");
        assert!(prepared.width <= 1_600);
        assert!(prepared.size <= 750_000);
        let decoded = ImageReader::with_format(
            BufReader::new(File::open(&output).unwrap()),
            ImageFormat::Jpeg,
        )
        .decode()
        .unwrap();
        assert!(!decoded.color().has_alpha());
    }

    #[test]
    fn exif_orientation_is_applied_and_metadata_is_not_copied() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("oriented.jpg");
        let output = dir.path().join("prepared");
        fixture_jpeg(&source, 40, 20);
        add_exif_orientation(&source, 6);

        let first = inspect_image_attachment(&source).unwrap();
        let second = inspect_image_attachment(&source).unwrap();
        assert_eq!((first.width, first.height), (Some(20), Some(40)));
        assert_eq!(
            first, second,
            "inspection must be deterministic and read-only"
        );

        let prepared = prepare_image_attachment(
            &source,
            &output,
            "oriented.jpg",
            ImageSizeProfile::Actual,
            128_000_000,
        )
        .unwrap();
        assert_eq!((prepared.width, prepared.height), (20, 40));
        let encoded = std::fs::read(output).unwrap();
        assert!(!encoded.windows(6).any(|window| window == b"Exif\0\0"));
    }

    #[test]
    fn malformed_input_and_output_limit_fail_closed() {
        let dir = TempDir::new().unwrap();
        let malformed = dir.path().join("malformed.jpg");
        std::fs::write(&malformed, b"not an image").unwrap();
        assert!(inspect_image_attachment(&malformed).is_err());

        let source = dir.path().join("source.jpg");
        fixture_jpeg(&source, 640, 480);
        assert!(matches!(
            prepare_image_attachment(
                &source,
                &dir.path().join("too-small"),
                "source.jpg",
                ImageSizeProfile::Actual,
                32,
            ),
            Err(ImageAttachmentError::OutputTooLarge)
        ));
    }

    #[test]
    fn animated_gif_is_rejected_before_transform() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("motion.gif");
        let mut encoder = GifEncoder::new(File::create(&source).unwrap());
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            let pixels = ImageBuffer::from_pixel(2, 2, Rgba(color));
            encoder
                .encode_frame(Frame::from_parts(
                    pixels,
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
        drop(encoder);
        let inspection = inspect_image_attachment(&source).unwrap();
        assert_eq!(inspection.disposition, ImageAttachmentDisposition::Animated);
        assert!(matches!(
            prepare_image_attachment(
                &source,
                &dir.path().join("prepared"),
                "motion.gif",
                ImageSizeProfile::Small,
                128_000_000,
            ),
            Err(ImageAttachmentError::Animated)
        ));
    }

    #[test]
    fn profiles_never_upscale() {
        assert_eq!(
            profile_dimensions(640, 480, ImageSizeProfile::Large),
            (640, 480)
        );
        assert_eq!(
            profile_dimensions(4_000, 2_000, ImageSizeProfile::Medium),
            (1_600, 800)
        );
    }

    #[test]
    fn animated_webp_flag_is_rejected_before_decode() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("animated.webp");
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&22u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBPVP8X");
        bytes.extend_from_slice(&10u32.to_le_bytes());
        bytes.push(0x02);
        bytes.extend_from_slice(&[0; 9]);
        std::fs::write(&path, bytes).unwrap();
        assert!(webp_has_animation(&path).unwrap());
    }
}
