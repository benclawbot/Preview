use std::ffi::CString;
use std::io::Cursor;
use std::path::Path;

use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{ExtendedColorType, ImageDecoder, ImageEncoder};
use libheif_rs::{
    Channel, ColorProfile, ColorSpace, CompressionFormat, EncoderQuality, HeifContext, LibHeif,
    RgbChroma,
};

use crate::error::{AppError, Result};
use crate::export;
use crate::model::{ImageInfo, MetadataEntry, MetadataReport, Operation, Rect, SaveOptions};
use crate::photo_meta::PhotoMetadata;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormatKind {
    Png,
    Jpeg,
    Tiff,
    Gif,
    WebP,
    Heic,
    Bmp,
    Jpeg2000,
    Other,
}

impl ImageFormatKind {
    pub fn label(self) -> &'static str {
        match self {
            ImageFormatKind::Png => "PNG",
            ImageFormatKind::Jpeg => "JPEG",
            ImageFormatKind::Tiff => "TIFF",
            ImageFormatKind::Gif => "GIF",
            ImageFormatKind::WebP => "WebP",
            ImageFormatKind::Heic => "HEIC",
            ImageFormatKind::Bmp => "BMP",
            ImageFormatKind::Jpeg2000 => "JPEG 2000",
            ImageFormatKind::Other => "Image",
        }
    }

    pub fn from_image_format(format: image::ImageFormat) -> Self {
        match format {
            image::ImageFormat::Png => ImageFormatKind::Png,
            image::ImageFormat::Jpeg => ImageFormatKind::Jpeg,
            image::ImageFormat::Tiff => ImageFormatKind::Tiff,
            image::ImageFormat::Gif => ImageFormatKind::Gif,
            image::ImageFormat::WebP => ImageFormatKind::WebP,
            image::ImageFormat::Bmp => ImageFormatKind::Bmp,
            _ => ImageFormatKind::Other,
        }
    }
}

pub fn detect_format(bytes: &[u8], path: &Path) -> ImageFormatKind {
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        if matches!(
            brand,
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1" | b"heim" | b"heis"
        ) {
            return ImageFormatKind::Heic;
        }
        if brand == b"avif" || brand == b"avis" {
            return ImageFormatKind::Heic;
        }
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return ImageFormatKind::Png;
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        return ImageFormatKind::Jpeg;
    }
    if bytes.starts_with(b"GIF8") {
        return ImageFormatKind::Gif;
    }
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        return ImageFormatKind::Tiff;
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return ImageFormatKind::WebP;
    }
    if bytes.starts_with(b"BM") {
        return ImageFormatKind::Bmp;
    }
    if bytes.starts_with(&[0xff, 0x4f, 0xff, 0x51])
        || (bytes.len() >= 12 && &bytes[4..12] == b"jP  \r\n\x87\n")
    {
        return ImageFormatKind::Jpeg2000;
    }
    if is_raw_path(path) {
        return ImageFormatKind::Other;
    }
    if let Some(format) = image::ImageFormat::from_path(path).ok() {
        if format != image::ImageFormat::Gif && format != image::ImageFormat::Png {
            // Fall back to the extension only when the signature was inconclusive.
        }
    }
    image::guess_format(bytes)
        .map(ImageFormatKind::from_image_format)
        .unwrap_or(ImageFormatKind::Other)
}

fn exif_file_type(format: ImageFormatKind) -> Option<little_exif::filetype::FileExtension> {
    use little_exif::filetype::FileExtension;
    match format {
        ImageFormatKind::Png => Some(FileExtension::PNG {
            as_zTXt_chunk: true,
        }),
        ImageFormatKind::Jpeg => Some(FileExtension::JPEG),
        ImageFormatKind::Tiff => Some(FileExtension::TIFF),
        ImageFormatKind::WebP => Some(FileExtension::WEBP),
        ImageFormatKind::Heic => Some(FileExtension::HEIF),
        _ => None,
    }
}

fn decode_icc_profile(bytes: &[u8], format: ImageFormatKind) -> Option<Vec<u8>> {
    if format == ImageFormatKind::Jpeg2000 {
        return jp2_icc_profile(bytes);
    }
    if format == ImageFormatKind::Tiff {
        return tiff_icc_profile(bytes);
    }
    if format == ImageFormatKind::Heic {
        let context = HeifContext::read_from_bytes(bytes).ok()?;
        let handle = context.primary_image_handle().ok()?;
        let profile = handle.color_profile_raw()?;
        return matches!(
            profile.profile_type(),
            libheif_rs::color_profile_types::PROF | libheif_rs::color_profile_types::R_ICC
        )
        .then_some(profile.data);
    }
    let image_format = match format {
        ImageFormatKind::Jpeg => image::ImageFormat::Jpeg,
        ImageFormatKind::Png => image::ImageFormat::Png,
        ImageFormatKind::Tiff => image::ImageFormat::Tiff,
        ImageFormatKind::WebP => image::ImageFormat::WebP,
        _ => return None,
    };
    let reader = image::ImageReader::with_format(Cursor::new(bytes), image_format);
    reader.into_decoder().ok()?.icc_profile().ok().flatten()
}

fn tiff_icc_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    let little_endian = match bytes.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |offset: usize| -> Option<u16> {
        let pair: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
        Some(if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        })
    };
    let u32_at = |offset: usize| -> Option<u32> {
        let word: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(word)
        } else {
            u32::from_be_bytes(word)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let mut ifd_offset = u32_at(4)? as usize;
    for _ in 0..256 {
        let entries = u16_at(ifd_offset)? as usize;
        let table_start = ifd_offset.checked_add(2)?;
        let table_bytes = entries.checked_mul(12)?;
        let table_end = table_start.checked_add(table_bytes)?;
        if table_end.checked_add(4)? > bytes.len() {
            return None;
        }
        for index in 0..entries {
            let entry = table_start.checked_add(index.checked_mul(12)?)?;
            if u16_at(entry)? != 34675 {
                continue;
            }
            let kind = u16_at(entry + 2)?;
            if !matches!(kind, 1 | 7) {
                return None;
            }
            let length = u32_at(entry + 4)? as usize;
            if length == 0 || length > 16 * 1024 * 1024 {
                return None;
            }
            let value_or_offset = entry + 8;
            let data_offset = if length <= 4 {
                value_or_offset
            } else {
                u32_at(value_or_offset)? as usize
            };
            let data_end = data_offset.checked_add(length)?;
            return Some(bytes.get(data_offset..data_end)?.to_vec());
        }
        ifd_offset = u32_at(table_end)? as usize;
        if ifd_offset == 0 {
            return None;
        }
    }
    None
}

fn jp2_icc_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    fn find_colr(bytes: &[u8], mut cursor: usize, end: usize) -> Option<Vec<u8>> {
        while cursor.checked_add(8)? <= end {
            let size32 = u32::from_be_bytes(bytes.get(cursor..cursor + 4)?.try_into().ok()?) as u64;
            let kind = bytes.get(cursor + 4..cursor + 8)?;
            let (size, header) = match size32 {
                0 => ((end - cursor) as u64, 8usize),
                1 => (
                    u64::from_be_bytes(bytes.get(cursor + 8..cursor + 16)?.try_into().ok()?),
                    16usize,
                ),
                value => (value, 8usize),
            };
            if size < header as u64 || size > (end - cursor) as u64 {
                return None;
            }
            let box_end = cursor.checked_add(size as usize)?;
            if kind == b"colr" {
                let payload = bytes.get(cursor + header..box_end)?;
                if payload.len() >= 4 && payload[0] == 2 {
                    return Some(payload[3..].to_vec());
                }
            } else if kind == b"jp2h" {
                if let Some(profile) = find_colr(bytes, cursor + header, box_end) {
                    return Some(profile);
                }
            }
            cursor = box_end;
        }
        None
    }
    let profile = find_colr(bytes, 0, bytes.len())?;
    (!profile.is_empty()).then_some(profile)
}

fn with_jp2_icc_profile(mut bytes: Vec<u8>, profile: &[u8]) -> Result<Vec<u8>> {
    if profile.is_empty() {
        return Ok(bytes);
    }
    fn locate(bytes: &[u8], mut cursor: usize, end: usize) -> Option<(usize, usize, usize, usize)> {
        while cursor.checked_add(8)? <= end {
            let size32 = u32::from_be_bytes(bytes.get(cursor..cursor + 4)?.try_into().ok()?) as u64;
            let kind = bytes.get(cursor + 4..cursor + 8)?;
            let (size, header) = match size32 {
                0 => ((end - cursor) as u64, 8usize),
                1 => (
                    u64::from_be_bytes(bytes.get(cursor + 8..cursor + 16)?.try_into().ok()?),
                    16usize,
                ),
                value => (value, 8usize),
            };
            if size < header as u64 || size > (end - cursor) as u64 {
                return None;
            }
            let box_end = cursor.checked_add(size as usize)?;
            if kind == b"jp2h" {
                let parent_start = cursor;
                let mut child = cursor + header;
                while child.checked_add(8)? <= box_end {
                    let child_size =
                        u32::from_be_bytes(bytes.get(child..child + 4)?.try_into().ok()?) as usize;
                    if child_size < 8 || child_size > box_end - child {
                        return None;
                    }
                    if bytes.get(child + 4..child + 8)? == b"colr" {
                        return Some((parent_start, child, child + child_size, box_end));
                    }
                    child += child_size;
                }
            }
            cursor = box_end;
        }
        None
    }
    let (parent_start, colr_start, colr_end, _) =
        locate(&bytes, 0, bytes.len()).ok_or_else(|| {
            AppError::Unsupported("JPEG 2000 file has no editable color specification box".into())
        })?;
    let colr_length = profile
        .len()
        .checked_add(11)
        .filter(|length| *length <= u32::MAX as usize)
        .ok_or_else(|| AppError::Message("ICC profile is too large for JPEG 2000".into()))?;
    let parent_length =
        u32::from_be_bytes(bytes[parent_start..parent_start + 4].try_into().unwrap()) as usize;
    let new_parent_length = parent_length
        .checked_sub(colr_end - colr_start)
        .and_then(|length| length.checked_add(colr_length))
        .filter(|length| *length <= u32::MAX as usize)
        .ok_or_else(|| AppError::Message("JPEG 2000 color header is too large".into()))?;
    let mut colr = Vec::with_capacity(colr_length);
    colr.extend_from_slice(&(colr_length as u32).to_be_bytes());
    colr.extend_from_slice(b"colr");
    colr.extend_from_slice(&[2, 0, 0]); // ICC profile method, precedence, approximation
    colr.extend_from_slice(profile);
    bytes.splice(colr_start..colr_end, colr);
    bytes[parent_start..parent_start + 4]
        .copy_from_slice(&(new_parent_length as u32).to_be_bytes());
    Ok(bytes)
}

fn decode_heif(bytes: &[u8]) -> Result<image::RgbaImage> {
    let context = HeifContext::read_from_bytes(bytes)
        .map_err(|error| AppError::Message(format!("could not read HEIF: {error}")))?;
    let handle = context
        .primary_image_handle()
        .map_err(|error| AppError::Message(format!("could not read HEIF image: {error}")))?;
    let has_alpha = handle.has_alpha_channel();
    let color_space = if has_alpha {
        ColorSpace::Rgb(RgbChroma::Rgba)
    } else {
        ColorSpace::Rgb(RgbChroma::Rgb)
    };
    let lib_heif = LibHeif::new();
    let image = lib_heif
        .decode(&handle, color_space, None)
        .map_err(|error| AppError::Message(format!("could not decode HEIF: {error}")))?;
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return Err(AppError::Message("HEIF image has no pixels".into()));
    }
    let planes = image.planes();
    let mut out = image::RgbaImage::new(width, height);
    if let Some(interleaved) = planes.interleaved.as_ref() {
        if interleaved.bits_per_pixel != 8 {
            return Err(AppError::Unsupported(
                "16-bit HEIF images are not supported yet".into(),
            ));
        }
        let channels = if has_alpha { 4 } else { 3 };
        for y in 0..height {
            for x in 0..width {
                let offset = y as usize * interleaved.stride + x as usize * channels;
                let data = interleaved.data;
                if offset + channels <= data.len() {
                    let r = data[offset];
                    let g = data[offset + 1];
                    let b = data[offset + 2];
                    let a = if has_alpha { data[offset + 3] } else { 255 };
                    out.put_pixel(x, y, image::Rgba([r, g, b, a]));
                }
            }
        }
        return Ok(out);
    }

    let (Some(red), Some(green), Some(blue)) =
        (planes.r.as_ref(), planes.g.as_ref(), planes.b.as_ref())
    else {
        return Err(AppError::Message(
            "HEIF decoder returned no RGB planes".into(),
        ));
    };
    if red.bits_per_pixel != 8 {
        return Err(AppError::Unsupported(
            "16-bit HEIF images are not supported yet".into(),
        ));
    }
    for y in 0..height {
        for x in 0..width {
            let offset_r = y as usize * red.stride + x as usize;
            let offset_g = y as usize * green.stride + x as usize;
            let offset_b = y as usize * blue.stride + x as usize;
            if offset_r < red.data.len()
                && offset_g < green.data.len()
                && offset_b < blue.data.len()
            {
                out.put_pixel(
                    x,
                    y,
                    image::Rgba([
                        red.data[offset_r],
                        green.data[offset_g],
                        blue.data[offset_b],
                        255,
                    ]),
                );
            }
        }
    }
    Ok(out)
}

fn decode_gif(bytes: &[u8]) -> Result<(image::RgbaImage, bool)> {
    let decoder = GifDecoder::new(Cursor::new(bytes))
        .map_err(|error| AppError::Message(format!("could not read GIF: {error}")))?;
    let frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|error| AppError::Message(format!("could not decode GIF frames: {error}")))?;
    let animated = frames.len() > 1;
    let first = frames
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Message("GIF has no frames".into()))?;
    Ok((first.into_buffer(), animated))
}

fn is_raw_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "cr2" | "nef" | "arw" | "dng"
            )
        })
}

fn decode_jpeg2000(bytes: &[u8]) -> Result<image::RgbaImage> {
    let decoded = jpeg2k::Image::from_bytes(bytes)
        .and_then(|image| image.get_pixels(Some(255)))
        .map_err(|error| AppError::Message(format!("could not decode JPEG 2000: {error}")))?;
    let rgba = match decoded.data {
        jpeg2k::ImagePixelData::Rgba8(data) => {
            image::RgbaImage::from_raw(decoded.width, decoded.height, data)
        }
        jpeg2k::ImagePixelData::Rgb8(data) => {
            image::RgbImage::from_raw(decoded.width, decoded.height, data)
                .map(|rgb| image::DynamicImage::ImageRgb8(rgb).to_rgba8())
        }
        jpeg2k::ImagePixelData::La8(data) => {
            image::GrayAlphaImage::from_raw(decoded.width, decoded.height, data)
                .map(|gray| image::DynamicImage::ImageLumaA8(gray).to_rgba8())
        }
        jpeg2k::ImagePixelData::L8(data) => {
            image::GrayImage::from_raw(decoded.width, decoded.height, data)
                .map(|gray| image::DynamicImage::ImageLuma8(gray).to_rgba8())
        }
        jpeg2k::ImagePixelData::Rgba16(data) => image::RgbaImage::from_raw(
            decoded.width,
            decoded.height,
            data.into_iter().map(|v| (v >> 8) as u8).collect(),
        ),
        jpeg2k::ImagePixelData::Rgb16(data) => image::RgbImage::from_raw(
            decoded.width,
            decoded.height,
            data.into_iter().map(|v| (v >> 8) as u8).collect(),
        )
        .map(|rgb| image::DynamicImage::ImageRgb8(rgb).to_rgba8()),
        jpeg2k::ImagePixelData::La16(data) => image::GrayAlphaImage::from_raw(
            decoded.width,
            decoded.height,
            data.into_iter().map(|v| (v >> 8) as u8).collect(),
        )
        .map(|gray| image::DynamicImage::ImageLumaA8(gray).to_rgba8()),
        jpeg2k::ImagePixelData::L16(data) => image::GrayImage::from_raw(
            decoded.width,
            decoded.height,
            data.into_iter().map(|v| (v >> 8) as u8).collect(),
        )
        .map(|gray| image::DynamicImage::ImageLuma8(gray).to_rgba8()),
    };
    rgba.ok_or_else(|| AppError::Message("unsupported JPEG 2000 pixel layout".into()))
}

fn encode_jpeg2000(
    image: &image::RgbaImage,
    icc_profile: Option<&[u8]>,
    raw_codestream: bool,
) -> Result<Vec<u8>> {
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return Err(AppError::Message("JPEG 2000 image has no pixels".into()));
    }
    let component = openjpeg_sys::opj_image_cmptparm_t {
        dx: 1,
        dy: 1,
        w: width,
        h: height,
        x0: 0,
        y0: 0,
        prec: 8,
        bpp: 8,
        sgnd: 0,
    };
    let mut components = [component; 3];
    let image_ptr = unsafe {
        openjpeg_sys::opj_image_create(
            components.len() as u32,
            components.as_mut_ptr(),
            openjpeg_sys::OPJ_COLOR_SPACE::OPJ_CLRSPC_SRGB,
        )
    };
    if image_ptr.is_null() {
        return Err(AppError::Other("could not allocate JPEG 2000 image".into()));
    }
    let rgb = flatten_on_white(image);
    let image_components = unsafe { std::slice::from_raw_parts_mut((*image_ptr).comps, 3) };
    for (channel, component) in image_components.iter_mut().enumerate() {
        if component.data.is_null() {
            unsafe { openjpeg_sys::opj_image_destroy(image_ptr) };
            return Err(AppError::Other(
                "JPEG 2000 encoder could not allocate pixel data".into(),
            ));
        }
        let data = unsafe {
            std::slice::from_raw_parts_mut(component.data, (width as usize) * (height as usize))
        };
        for (value, pixel) in data.iter_mut().zip(rgb.chunks_exact(3)) {
            *value = pixel[channel] as i32;
        }
    }
    unsafe {
        (*image_ptr).x1 = width;
        (*image_ptr).y1 = height;
    }
    let mut parameters = unsafe { std::mem::zeroed::<openjpeg_sys::opj_cparameters_t>() };
    unsafe { openjpeg_sys::opj_set_default_encoder_parameters(&mut parameters) };
    parameters.tcp_numlayers = 1;
    parameters.cp_disto_alloc = 1;
    parameters.tcp_rates[0] = 0.0; // lossless
    parameters.irreversible = 0;
    parameters.numresolution = ((width.min(height) as f32).log2().floor() as i32 + 1).clamp(1, 6);
    let staging = tempfile::tempdir()?;
    let output = staging.path().join(if raw_codestream {
        "image.j2k"
    } else {
        "image.jp2"
    });
    let output_name = CString::new(output.to_string_lossy().as_bytes())
        .map_err(|_| AppError::Message("JPEG 2000 output path is invalid".into()))?;
    let format = if raw_codestream {
        openjpeg_sys::OPJ_CODEC_FORMAT::OPJ_CODEC_J2K
    } else {
        openjpeg_sys::OPJ_CODEC_FORMAT::OPJ_CODEC_JP2
    };
    parameters.cod_format = if raw_codestream { 0 } else { 1 };
    let codec = unsafe { openjpeg_sys::opj_create_compress(format) };
    if codec.is_null() {
        unsafe { openjpeg_sys::opj_image_destroy(image_ptr) };
        return Err(AppError::Other("could not create JPEG 2000 encoder".into()));
    }
    let stream =
        unsafe { openjpeg_sys::opj_stream_create_default_file_stream(output_name.as_ptr(), 0) };
    if stream.is_null() {
        unsafe {
            openjpeg_sys::opj_destroy_codec(codec);
            openjpeg_sys::opj_image_destroy(image_ptr);
        }
        return Err(AppError::Other(
            "could not create JPEG 2000 output stream".into(),
        ));
    }
    let success = unsafe {
        openjpeg_sys::opj_setup_encoder(codec, &mut parameters, image_ptr) != 0
            && openjpeg_sys::opj_start_compress(codec, image_ptr, stream) != 0
            && openjpeg_sys::opj_encode(codec, stream) != 0
            && openjpeg_sys::opj_end_compress(codec, stream) != 0
    };
    unsafe {
        openjpeg_sys::opj_stream_destroy(stream);
        openjpeg_sys::opj_destroy_codec(codec);
        openjpeg_sys::opj_image_destroy(image_ptr);
    }
    if !success {
        return Err(AppError::Other("JPEG 2000 encoding failed".into()));
    }
    let bytes = std::fs::read(output)?;
    if bytes.is_empty() {
        return Err(AppError::Other(
            "JPEG 2000 encoder produced an empty file".into(),
        ));
    }
    if let Some(profile) = icc_profile {
        if raw_codestream {
            return Err(AppError::Unsupported(
                "raw JPEG 2000 codestream export cannot retain the ICC profile; export as JP2 instead".into(),
            ));
        }
        with_jp2_icc_profile(bytes, profile)
    } else {
        Ok(bytes)
    }
}

fn demosaic_raw(raw: &rawloader::RawImage) -> Result<image::RgbaImage> {
    if raw.width == 0 || raw.height == 0 || raw.width.saturating_mul(raw.height) > 150_000_000 {
        return Err(AppError::Message(
            "RAW image dimensions are invalid or exceed 150 megapixels".into(),
        ));
    }
    let values: Vec<f32> = match &raw.data {
        rawloader::RawImageData::Integer(values) => values.iter().map(|v| *v as f32).collect(),
        rawloader::RawImageData::Float(values) => values.clone(),
    };
    let width = raw.width;
    let height = raw.height;
    if raw.cpp >= 3 {
        let mut output = image::RgbaImage::new(width as u32, height as u32);
        let max = values.iter().copied().fold(0.0f32, f32::max).max(1.0);
        for y in 0..height {
            for x in 0..width {
                let offset = (y * width + x) * raw.cpp;
                let channel = |i: usize| {
                    (values.get(offset + i).copied().unwrap_or(0.0) / max * 255.0).clamp(0.0, 255.0)
                        as u8
                };
                output.put_pixel(
                    x as u32,
                    y as u32,
                    image::Rgba([channel(0), channel(1), channel(2), 255]),
                );
            }
        }
        return Ok(output);
    }
    let channels = [0usize, 1, 2];
    let white = raw.whitelevels.iter().copied().max().unwrap_or(u16::MAX) as f32;
    let black = raw.blacklevels.iter().copied().min().unwrap_or(0) as f32;
    let span = (white - black).max(1.0);
    let mut output = image::RgbaImage::new(width as u32, height as u32);
    for y in 0..height {
        for x in 0..width {
            let mut sums = [0.0f32; 3];
            let mut counts = [0.0f32; 3];
            let y0 = y.saturating_sub(2);
            let y1 = (y + 2).min(height - 1);
            let x0 = x.saturating_sub(2);
            let x1 = (x + 2).min(width - 1);
            for sy in y0..=y1 {
                for sx in x0..=x1 {
                    let color = raw.cfa.color_at(sy, sx).min(2);
                    let sample = values.get(sy * width + sx).copied().unwrap_or(black);
                    sums[color] += sample;
                    counts[color] += 1.0;
                }
            }
            let mut rgb = [0u8; 3];
            for index in channels {
                let value = if counts[index] > 0.0 {
                    sums[index] / counts[index]
                } else {
                    black
                };
                let wb = raw.wb_coeffs[index].max(0.05);
                rgb[index] = (((value - black) / span * wb * 2.1).clamp(0.0, 1.0) * 255.0) as u8;
            }
            output.put_pixel(
                x as u32,
                y as u32,
                image::Rgba([rgb[0], rgb[1], rgb[2], 255]),
            );
        }
    }
    Ok(output)
}

fn embedded_jpeg_preview(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut best: Option<(u64, Vec<u8>)> = None;
    let mut start = 0usize;
    while start + 4 < bytes.len() {
        let Some(relative) = bytes[start..]
            .windows(2)
            .position(|marker| marker == [0xff, 0xd8])
        else {
            break;
        };
        let soi = start + relative;
        let Some(end_relative) = bytes[soi + 2..]
            .windows(2)
            .position(|marker| marker == [0xff, 0xd9])
        else {
            start = soi + 2;
            continue;
        };
        let end = soi + 2 + end_relative + 2;
        let candidate = &bytes[soi..end];
        if let Ok(decoded) = image::load_from_memory(candidate) {
            let size = decoded.width() as u64 * decoded.height() as u64;
            if size > best.as_ref().map(|(best_size, _)| *best_size).unwrap_or(0)
                && size <= 150_000_000
            {
                best = Some((size, candidate.to_vec()));
            }
        }
        start = end;
    }
    best.map(|(_, bytes)| bytes)
}

pub struct ImageState {
    pub source: image::RgbaImage,
    pub current: image::RgbaImage,
    pub ops: Vec<Operation>,
    pub redo: Vec<Operation>,
    pub format: ImageFormatKind,
    pub original: Vec<u8>,
    pub animated: bool,
    pub has_alpha: bool,
    pub orientation: u16,
    pub meta: Option<PhotoMetadata>,
    pub icc_profile: Option<Vec<u8>>,
}

impl ImageState {
    pub fn open(bytes: Vec<u8>, path: &Path) -> Result<Self> {
        let format = detect_format(&bytes, path);
        let file_type = exif_file_type(format);
        let meta = file_type.and_then(|file_type| PhotoMetadata::load(&bytes, file_type));
        let icc_profile = decode_icc_profile(&bytes, format);
        let orientation = meta.as_ref().map(|meta| meta.orientation()).unwrap_or(1);

        let (mut decoded, animated) = match format {
            ImageFormatKind::Heic => (image::DynamicImage::ImageRgba8(decode_heif(&bytes)?), false),
            ImageFormatKind::Jpeg2000 => (
                image::DynamicImage::ImageRgba8(decode_jpeg2000(&bytes)?),
                false,
            ),
            ImageFormatKind::Other if is_raw_path(path) => {
                if let Some(preview) = embedded_jpeg_preview(&bytes) {
                    let decoded = image::load_from_memory(&preview).map_err(|error| {
                        AppError::Message(format!("could not decode embedded RAW preview: {error}"))
                    })?;
                    (decoded, false)
                } else {
                    let raw = rawloader::decode(&mut Cursor::new(&bytes)).map_err(|error| {
                        AppError::Message(format!("could not decode RAW image: {error}"))
                    })?;
                    (image::DynamicImage::ImageRgba8(demosaic_raw(&raw)?), false)
                }
            }
            ImageFormatKind::Gif => {
                let (frame, animated) = decode_gif(&bytes)?;
                (image::DynamicImage::ImageRgba8(frame), animated)
            }
            _ => {
                let image = image::load_from_memory(&bytes).map_err(|error| {
                    AppError::Message(format!("could not decode image: {error}"))
                })?;
                (image, false)
            }
        };

        // libheif already applies container transformations (irot/imir). Other
        // formats advertise orientation through EXIF, which we bake into pixels.
        if format != ImageFormatKind::Heic && orientation > 1 {
            if let Some(orientation) = image::metadata::Orientation::from_exif(orientation as u8) {
                decoded.apply_orientation(orientation);
            }
        }

        let has_alpha = decoded.color().has_alpha();
        let source = decoded.to_rgba8();
        if source.width() == 0 || source.height() == 0 {
            return Err(AppError::Message("image has no pixels".into()));
        }
        if (source.width() as u64) * (source.height() as u64) > 150_000_000 {
            return Err(AppError::Message(
                "image is larger than the 150 megapixel MVP limit".into(),
            ));
        }

        Ok(Self {
            current: source.clone(),
            source,
            ops: Vec::new(),
            redo: Vec::new(),
            format,
            original: bytes,
            animated,
            has_alpha,
            orientation,
            meta,
            icc_profile,
        })
    }

    pub fn info(&self) -> ImageInfo {
        ImageInfo {
            width: self.current.width(),
            height: self.current.height(),
            format: self.format.label().to_string(),
            animated: self.animated,
            has_alpha: self.has_alpha || self.current.pixels().any(|pixel| pixel[3] < 255),
            orientation: self.orientation,
        }
    }

    pub fn is_pristine(&self) -> bool {
        self.ops.is_empty() && self.redo.is_empty()
    }

    pub fn apply(&mut self, operation: Operation) -> Result<()> {
        let next = apply_to_image(&self.current, &operation)?;
        self.current = next;
        self.ops.push(operation);
        self.redo.clear();
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(operation) = self.ops.pop() else {
            return false;
        };
        self.current = self.recompute();
        self.redo.push(operation);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(operation) = self.redo.pop() else {
            return false;
        };
        match apply_to_image(&self.current, &operation) {
            Ok(next) => {
                self.current = next;
                self.ops.push(operation);
                true
            }
            Err(_) => false,
        }
    }

    fn recompute(&self) -> image::RgbaImage {
        let mut current = self.source.clone();
        for operation in &self.ops {
            match apply_to_image(&current, operation) {
                Ok(next) => current = next,
                Err(_) => break,
            }
        }
        current
    }

    /// GIFs are shown from their original bytes while pristine so the browser
    /// animates all frames with the stored delays. Any edit switches to a PNG
    /// snapshot of the first frame.
    pub fn view_bytes(&self) -> Result<(Vec<u8>, &'static str)> {
        if self.format == ImageFormatKind::Gif && self.is_pristine() {
            return Ok((self.original.clone(), "image/gif"));
        }
        Ok((self.encode(ImageFormatKind::Png, 95)?, "image/png"))
    }

    pub fn encode(&self, format: ImageFormatKind, quality: u8) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        match format {
            ImageFormatKind::Png => {
                let mut encoder = PngEncoder::new(&mut out);
                if let Some(profile) = &self.icc_profile {
                    encoder.set_icc_profile(profile.clone()).map_err(|error| {
                        AppError::Message(format!("could not preserve ICC profile: {error}"))
                    })?;
                }
                encoder.write_image(
                    self.current.as_raw(),
                    self.current.width(),
                    self.current.height(),
                    ExtendedColorType::Rgba8,
                )?;
            }
            ImageFormatKind::Jpeg => {
                let rgb = flatten_on_white(&self.current);
                let mut encoder = JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
                if let Some(profile) = &self.icc_profile {
                    encoder.set_icc_profile(profile.clone()).map_err(|error| {
                        AppError::Message(format!("could not preserve ICC profile: {error}"))
                    })?;
                }
                encoder.write_image(
                    &rgb,
                    self.current.width(),
                    self.current.height(),
                    ExtendedColorType::Rgb8,
                )?;
            }
            ImageFormatKind::Tiff => {
                let mut encoder = TiffEncoder::new(Cursor::new(&mut out));
                if let Some(profile) = &self.icc_profile {
                    encoder.set_icc_profile(profile.clone()).map_err(|error| {
                        AppError::Message(format!("could not preserve ICC profile: {error}"))
                    })?;
                }
                encoder.write_image(
                    self.current.as_raw(),
                    self.current.width(),
                    self.current.height(),
                    ExtendedColorType::Rgba8,
                )?;
            }
            ImageFormatKind::Gif => {
                if self.icc_profile.is_some() {
                    return Err(AppError::Unsupported(
                        "GIF export cannot preserve this image's ICC profile; choose PNG, JPEG, TIFF, or WebP".into(),
                    ));
                }
                image::DynamicImage::ImageRgba8(self.current.clone())
                    .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Gif)?;
            }
            ImageFormatKind::WebP => {
                let mut encoder = WebPEncoder::new_lossless(&mut out);
                if let Some(profile) = &self.icc_profile {
                    encoder.set_icc_profile(profile.clone()).map_err(|error| {
                        AppError::Message(format!("could not preserve ICC profile: {error}"))
                    })?;
                }
                encoder.write_image(
                    self.current.as_raw(),
                    self.current.width(),
                    self.current.height(),
                    ExtendedColorType::Rgba8,
                )?;
            }
            ImageFormatKind::Heic => {
                return encode_heic(&self.current, quality, self.icc_profile.as_deref());
            }
            ImageFormatKind::Bmp => {
                if self.icc_profile.is_some() {
                    return Err(AppError::Unsupported(
                        "BMP export cannot preserve this image's ICC profile; choose PNG, JPEG, TIFF, or WebP".into(),
                    ));
                }
                image::DynamicImage::ImageRgba8(self.current.clone())
                    .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Bmp)?;
            }
            ImageFormatKind::Jpeg2000 => {
                return encode_jpeg2000(&self.current, self.icc_profile.as_deref(), false);
            }
            ImageFormatKind::Other => {
                return Err(AppError::Message("unknown export format".into()));
            }
        }
        Ok(out)
    }

    pub fn export(&mut self, path: &Path, options: &SaveOptions) -> Result<()> {
        if format_is_pdf_export(path, options.format.as_deref()) {
            let pdf = export::image_to_pdf(&self.current)?;
            std::fs::write(path, pdf)?;
            return Ok(());
        }

        let format = export_format_for_path(path, options.format.as_deref(), self.format)?;
        let quality = options.quality.unwrap_or(92);
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("psd"))
        {
            std::fs::write(path, encode_psd(&self.current))?;
            return Ok(());
        }
        let mut encoded = if format == ImageFormatKind::Jpeg2000 {
            let raw_codestream = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "j2k" | "j2c" | "jpc"
                    )
                });
            encode_jpeg2000(&self.current, self.icc_profile.as_deref(), raw_codestream)?
        } else {
            self.encode(format, quality)?
        };
        if let Some(meta) = &self.meta {
            let mut meta = PhotoMetadata {
                raw: meta.raw.clone(),
                file_type: meta.file_type,
            };
            if options.strip_gps {
                meta.strip_gps();
            }
            let changed_pixels = !self.ops.is_empty() || self.orientation > 1;
            if changed_pixels {
                meta.bake_orientation();
            }
            if !matches!(format, ImageFormatKind::Gif) {
                meta.write_to(&mut encoded)?;
            }
        } else if options.strip_gps {
            return Err(AppError::Message(
                "this image has no EXIF metadata to strip".into(),
            ));
        }

        std::fs::write(path, encoded)?;
        Ok(())
    }

    pub fn metadata(&self) -> MetadataReport {
        let mut entries = vec![
            MetadataEntry {
                group: "Image".into(),
                key: "Dimensions".into(),
                value: format!("{} × {} px", self.current.width(), self.current.height()),
            },
            MetadataEntry {
                group: "Image".into(),
                key: "Format".into(),
                value: self.format.label().into(),
            },
            MetadataEntry {
                group: "Image".into(),
                key: "Alpha channel".into(),
                value: if self.has_alpha { "yes" } else { "no" }.into(),
            },
            MetadataEntry {
                group: "Image".into(),
                key: "Animated".into(),
                value: if self.animated { "yes" } else { "no" }.into(),
            },
        ];
        if let Some(meta) = &self.meta {
            entries.extend(meta.entries());
        }
        let gps_present = self.meta.as_ref().is_some_and(|meta| meta.gps_present());
        MetadataReport {
            entries,
            can_strip_gps: gps_present,
            gps_present,
        }
    }
}

fn format_is_pdf_export(path: &Path, explicit: Option<&str>) -> bool {
    explicit
        .map(|format| format.eq_ignore_ascii_case("pdf"))
        .unwrap_or_else(|| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("pdf"))
                .unwrap_or(false)
        })
}

pub fn export_format_for_path(
    path: &Path,
    explicit: Option<&str>,
    fallback: ImageFormatKind,
) -> Result<ImageFormatKind> {
    if let Some(format) = explicit {
        return match format.to_ascii_lowercase().as_str() {
            "pdf" => Ok(ImageFormatKind::Other),
            "png" => Ok(ImageFormatKind::Png),
            "jpg" | "jpeg" => Ok(ImageFormatKind::Jpeg),
            "tif" | "tiff" => Ok(ImageFormatKind::Tiff),
            "gif" => Ok(ImageFormatKind::Gif),
            "webp" => Ok(ImageFormatKind::WebP),
            "heic" | "heif" => Ok(ImageFormatKind::Heic),
            "jp2" | "j2k" | "jpf" => Ok(ImageFormatKind::Jpeg2000),
            "psd" => Ok(ImageFormatKind::Other),
            other => Err(AppError::Message(format!(
                "unsupported export format: {other}"
            ))),
        };
    }
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => Ok(ImageFormatKind::Other),
        "png" => Ok(ImageFormatKind::Png),
        "jpg" | "jpeg" => Ok(ImageFormatKind::Jpeg),
        "tif" | "tiff" => Ok(ImageFormatKind::Tiff),
        "gif" => Ok(ImageFormatKind::Gif),
        "webp" => Ok(ImageFormatKind::WebP),
        "heic" | "heif" => Ok(ImageFormatKind::Heic),
        "jp2" | "j2k" | "jpf" => Ok(ImageFormatKind::Jpeg2000),
        "psd" => Ok(ImageFormatKind::Other),
        "" => Ok(fallback),
        other => Err(AppError::Message(format!(
            "unknown file extension: {other}"
        ))),
    }
}

fn apply_to_image(image: &image::RgbaImage, operation: &Operation) -> Result<image::RgbaImage> {
    match operation {
        Operation::CropImage {
            x,
            y,
            width,
            height,
        } => {
            let x = (*x).min(image.width().saturating_sub(1));
            let y = (*y).min(image.height().saturating_sub(1));
            let width = (*width).min(image.width() - x).max(1);
            let height = (*height).min(image.height() - y).max(1);
            Ok(image::imageops::crop_imm(image, x, y, width, height).to_image())
        }
        Operation::ResizeImage { width, height } => {
            if *width == 0 || *height == 0 {
                return Err(AppError::Message(
                    "resize dimensions must be positive".into(),
                ));
            }
            if (*width as u64) * (*height as u64) > 150_000_000 {
                return Err(AppError::Message(
                    "resized image would exceed the 150 megapixel limit".into(),
                ));
            }
            Ok(image::imageops::resize(
                image,
                *width,
                *height,
                FilterType::Lanczos3,
            ))
        }
        Operation::RotateImage { degrees } => {
            let normalized = degrees.rem_euclid(360);
            let rounded = ((normalized as f32 / 90.0).round() as i32 * 90).rem_euclid(360);
            match rounded {
                0 => Ok(image.clone()),
                90 => Ok(image::imageops::rotate90(image)),
                180 => Ok(image::imageops::rotate180(image)),
                270 => Ok(image::imageops::rotate270(image)),
                _ => Err(AppError::Message(
                    "rotation must be a multiple of 90°".into(),
                )),
            }
        }
        Operation::FlipImage { horizontal } => Ok(if *horizontal {
            image::imageops::flip_horizontal(image)
        } else {
            image::imageops::flip_vertical(image)
        }),
        Operation::AdjustImage {
            brightness,
            contrast,
            saturation,
            sharpen,
        } => {
            if ![brightness, contrast, saturation, sharpen]
                .iter()
                .all(|v| v.is_finite())
            {
                return Err(AppError::Message(
                    "image adjustment values must be finite".into(),
                ));
            }
            let mut out = image.clone();
            if brightness.abs() > 0.001 {
                out =
                    image::imageops::brighten(&out, brightness.clamp(-255.0, 255.0).round() as i32);
            }
            if contrast.abs() > 0.001 {
                out = image::imageops::contrast(&out, contrast.clamp(-100.0, 100.0));
            }
            if (saturation - 1.0).abs() > 0.001 {
                for pixel in out.pixels_mut() {
                    let gray = 0.2126 * pixel[0] as f32
                        + 0.7152 * pixel[1] as f32
                        + 0.0722 * pixel[2] as f32;
                    for channel in 0..3 {
                        pixel[channel] = (gray
                            + (pixel[channel] as f32 - gray) * saturation.clamp(0.0, 3.0))
                        .clamp(0.0, 255.0)
                        .round() as u8;
                    }
                }
            }
            if *sharpen > 0.001 {
                out = sharpen_image(&out, sharpen.clamp(0.0, 2.0));
            }
            Ok(out)
        }
        Operation::ImageLevels {
            black,
            white,
            gamma,
        } => {
            if ![black, white, gamma].iter().all(|v| v.is_finite())
                || *black < 0.0
                || *white > 1.0
                || black >= white
                || *gamma <= 0.0
            {
                return Err(AppError::Message(
                    "levels require 0 ≤ black < white ≤ 1 and positive gamma".into(),
                ));
            }
            let range = white - black;
            let inv_gamma = 1.0 / gamma.clamp(0.05, 10.0);
            let mut out = image.clone();
            for pixel in out.pixels_mut() {
                for channel in 0..3 {
                    let normalized = ((pixel[channel] as f32 / 255.0 - black) / range)
                        .clamp(0.0, 1.0)
                        .powf(inv_gamma);
                    pixel[channel] = (normalized * 255.0).round() as u8;
                }
            }
            Ok(out)
        }
        Operation::ImageCurves { points } => {
            if points.len() < 2
                || points.len() > 16
                || points.iter().any(|point| {
                    !point.x.is_finite()
                        || !point.y.is_finite()
                        || !(0.0..=1.0).contains(&point.x)
                        || !(0.0..=1.0).contains(&point.y)
                })
                || points[0].x != 0.0
                || points[points.len() - 1].x != 1.0
                || points.windows(2).any(|pair| pair[0].x >= pair[1].x)
            {
                return Err(AppError::Message(
                    "curves require 2–16 points with strictly increasing input values from 0 to 1"
                        .into(),
                ));
            }
            let mut curve = [0u8; 256];
            let mut segment = 0usize;
            for (input, output) in curve.iter_mut().enumerate() {
                let x = input as f32 / 255.0;
                while segment + 1 < points.len() - 1 && x > points[segment + 1].x {
                    segment += 1;
                }
                let a = points[segment];
                let b = points[segment + 1];
                let t = ((x - a.x) / (b.x - a.x)).clamp(0.0, 1.0);
                *output = ((a.y + (b.y - a.y) * t) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            let mut out = image.clone();
            for pixel in out.pixels_mut() {
                for channel in 0..3 {
                    pixel[channel] = curve[pixel[channel] as usize];
                }
            }
            Ok(out)
        }
        Operation::InstantAlpha { color, tolerance } => {
            let Some([r, g, b]) = parse_hex(color) else {
                return Err(AppError::Message(
                    "instant alpha requires #rrggbb color".into(),
                ));
            };
            let threshold = *tolerance as f32;
            let mut out = image.clone();
            for pixel in out.pixels_mut() {
                let distance = ((pixel[0] as f32 - r as f32).powi(2)
                    + (pixel[1] as f32 - g as f32).powi(2)
                    + (pixel[2] as f32 - b as f32).powi(2))
                .sqrt()
                    / 3.0f32.sqrt();
                if distance <= threshold {
                    pixel[3] = 0;
                }
            }
            Ok(out)
        }
        Operation::Redact { .. } => Err(AppError::Unsupported(
            "redaction ships in v1; it needs object-level removal".into(),
        )),
        _ => Err(AppError::Message(
            "this operation is not valid for an image".into(),
        )),
    }
}

fn parse_hex(value: &str) -> Option<[u8; 3]> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}

fn sharpen_image(image: &image::RgbaImage, amount: f32) -> image::RgbaImage {
    let mut out = image.clone();
    let width = image.width() as i32;
    let height = image.height() as i32;
    for y in 1..height.saturating_sub(1) {
        for x in 1..width.saturating_sub(1) {
            let center = image.get_pixel(x as u32, y as u32);
            let mut pixel = *center;
            for channel in 0..3 {
                let neighbors = image.get_pixel((x - 1) as u32, y as u32)[channel] as f32
                    + image.get_pixel((x + 1) as u32, y as u32)[channel] as f32
                    + image.get_pixel(x as u32, (y - 1) as u32)[channel] as f32
                    + image.get_pixel(x as u32, (y + 1) as u32)[channel] as f32;
                pixel[channel] = (center[channel] as f32
                    + amount * (center[channel] as f32 - neighbors / 4.0))
                    .clamp(0.0, 255.0)
                    .round() as u8;
            }
            out.put_pixel(x as u32, y as u32, pixel);
        }
    }
    out
}

fn encode_psd(image: &image::RgbaImage) -> Vec<u8> {
    let rgb = flatten_on_white(image);
    let mut out = Vec::with_capacity(64 + rgb.len());
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&(image.height() as u32).to_be_bytes());
    out.extend_from_slice(&(image.width() as u32).to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // color mode data
    out.extend_from_slice(&0u32.to_be_bytes()); // image resources
    out.extend_from_slice(&0u32.to_be_bytes()); // layer and mask data
    out.extend_from_slice(&0u16.to_be_bytes()); // raw planar channels
    let pixels = (image.width() * image.height()) as usize;
    for channel in 0..3 {
        for index in 0..pixels {
            out.push(rgb[index * 3 + channel]);
        }
    }
    out
}

fn encode_heic(
    image: &image::RgbaImage,
    quality: u8,
    icc_profile: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut context = HeifContext::new()
        .map_err(|error| AppError::Message(format!("HEIC encoder unavailable: {error}")))?;
    let libheif = LibHeif::new();
    let mut encoder = libheif
        .encoder_for_format(CompressionFormat::Hevc)
        .map_err(|error| {
            AppError::Message(format!(
                "HEIC encoder unavailable: {error}; install a libheif HEVC encoder plugin"
            ))
        })?;
    encoder
        .set_quality(EncoderQuality::Lossy(quality.clamp(1, 100)))
        .map_err(|error| AppError::Message(format!("could not configure HEIC encoder: {error}")))?;
    let mut heif_image = libheif_rs::Image::new(
        image.width(),
        image.height(),
        ColorSpace::Rgb(RgbChroma::Rgb),
    )
    .map_err(|error| AppError::Message(format!("could not create HEIC image: {error}")))?;
    heif_image
        .create_plane(Channel::Interleaved, image.width(), image.height(), 24)
        .map_err(|error| AppError::Message(format!("could not allocate HEIC pixels: {error}")))?;
    let plane = heif_image
        .planes_mut()
        .interleaved
        .ok_or_else(|| AppError::Message("HEIC encoder returned no pixel plane".into()))?;
    for y in 0..image.height() as usize {
        for x in 0..image.width() as usize {
            let src = image.get_pixel(x as u32, y as u32);
            let offset = y * plane.stride + x * 3;
            plane.data[offset..offset + 3].copy_from_slice(&src.0[..3]);
        }
    }
    if let Some(profile) = icc_profile {
        let raw = libheif_rs::ColorProfileRaw::new(
            libheif_rs::color_profile_types::PROF,
            profile.to_vec(),
        );
        heif_image.set_color_profile_raw(&raw).map_err(|error| {
            AppError::Message(format!("could not attach HEIC ICC profile: {error}"))
        })?;
    }
    context
        .encode_image(&heif_image, &mut encoder, None)
        .map_err(|error| AppError::Message(format!("HEIC encoding failed: {error}")))?;
    context
        .write_to_bytes()
        .map_err(|error| AppError::Message(format!("could not write HEIC: {error}")))
}

fn flatten_on_white(image: &image::RgbaImage) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((image.width() * image.height() * 3) as usize);
    for pixel in image.pixels() {
        let alpha = pixel[3] as f32 / 255.0;
        for channel in 0..3 {
            let value = pixel[channel] as f32 * alpha + 255.0 * (1.0 - alpha);
            rgb.push(value.round() as u8);
        }
    }
    rgb
}

#[allow(dead_code)]
fn rect_to_crop(rect: Rect, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let x = rect.x.max(0.0).round().min(width as f32) as u32;
    let y = rect.y.max(0.0).round().min(height as f32) as u32;
    let w = rect.width.max(1.0).round().min((width - x) as f32).max(1.0) as u32;
    let h = rect
        .height
        .max(1.0)
        .round()
        .min((height - y) as f32)
        .max(1.0) as u32;
    (x, y, w, h)
}
