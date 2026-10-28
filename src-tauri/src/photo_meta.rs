use std::io::Cursor;

use little_exif::endian::Endian;
use little_exif::exif_tag::ExifTag;
use little_exif::filetype::FileExtension;
use little_exif::ifd::ExifTagGroup;
use little_exif::metadata::Metadata;

use crate::error::{AppError, Result};
use crate::model::MetadataEntry;

/// EXIF handling for raster images.
///
/// `little_exif` reads and writes JPEG, PNG, TIFF, WebP and HEIF metadata. We
/// keep the original tag set, bake any display orientation into pixels, and
/// write orientation 1 back so viewers do not rotate the image twice.
pub struct PhotoMetadata {
    pub raw: Metadata,
    pub file_type: FileExtension,
}

impl PhotoMetadata {
    pub fn load(bytes: &[u8], file_type: FileExtension) -> Option<Self> {
        let mut cursor = Cursor::new(bytes);
        let detected = FileExtension::auto_detect(&mut cursor);
        if !matches!(
            (detected, file_type),
            (Some(FileExtension::PNG { .. }), FileExtension::PNG { .. })
                | (Some(FileExtension::JPEG), FileExtension::JPEG)
                | (Some(FileExtension::TIFF), FileExtension::TIFF)
                | (Some(FileExtension::WEBP), FileExtension::WEBP)
                | (Some(FileExtension::HEIF), FileExtension::HEIF)
        ) {
            return None;
        }
        let raw = Metadata::new_from_vec(&bytes.to_vec(), file_type).ok()?;
        Some(Self { raw, file_type })
    }

    fn u16_tag(&self, hex: u16, group: ExifTagGroup) -> Option<u16> {
        let tag = self.raw.get_tag_by_hex(hex, Some(group)).next()?;
        let bytes = tag.value_as_u8_vec(&self.raw.get_endian());
        if bytes.len() < 2 {
            return None;
        }
        Some(match self.raw.get_endian() {
            Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
            Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
        })
    }

    pub fn orientation(&self) -> u16 {
        self.u16_tag(0x0112, ExifTagGroup::GENERIC)
            .filter(|value| (1..=8).contains(value))
            .unwrap_or(1)
    }

    fn rational_tag(&self, hex: u16, group: ExifTagGroup, index: usize) -> Option<f64> {
        let tag = self.raw.get_tag_by_hex(hex, Some(group)).next()?;
        let bytes = tag.value_as_u8_vec(&self.raw.get_endian());
        let offset = index * 8;
        if bytes.len() < offset + 8 {
            return None;
        }
        let read = |slice: &[u8]| -> u32 {
            match self.raw.get_endian() {
                Endian::Little => u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]),
                Endian::Big => u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]),
            }
        };
        let numerator = read(&bytes[offset..offset + 4]) as f64;
        let denominator = read(&bytes[offset + 4..offset + 8]) as f64;
        (denominator != 0.0).then_some(numerator / denominator)
    }

    fn string_tag(&self, hex: u16, group: ExifTagGroup) -> Option<String> {
        let tag = self.raw.get_tag_by_hex(hex, Some(group)).next()?;
        let bytes = tag.value_as_u8_vec(&self.raw.get_endian());
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        let value = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
        (!value.is_empty()).then_some(value)
    }

    pub fn gps_present(&self) -> bool {
        self.raw
            .get_ifds()
            .iter()
            .any(|ifd| ifd.get_ifd_type() == ExifTagGroup::GPS && !ifd.get_tags().is_empty())
            || self
                .raw
                .get_ifd(ExifTagGroup::GENERIC, 0)
                .is_some_and(|ifd| ifd.get_tags().iter().any(|tag| tag.as_u16() == 0x8825))
    }

    fn gps_coordinate(&self, value_hex: u16, ref_hex: u16) -> Option<String> {
        let degrees = self.rational_tag(value_hex, ExifTagGroup::GPS, 0)?;
        let minutes = self
            .rational_tag(value_hex, ExifTagGroup::GPS, 1)
            .unwrap_or(0.0);
        let seconds = self
            .rational_tag(value_hex, ExifTagGroup::GPS, 2)
            .unwrap_or(0.0);
        let reference = self
            .string_tag(ref_hex, ExifTagGroup::GPS)
            .unwrap_or_default();
        Some(format!(
            "{degrees:.0}° {minutes:.0}′ {seconds:.2}″ {reference}"
        ))
    }

    pub fn entries(&self) -> Vec<MetadataEntry> {
        let mut entries = Vec::new();
        let mut push = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                if !value.is_empty() {
                    entries.push(MetadataEntry {
                        group: "EXIF".into(),
                        key: key.into(),
                        value,
                    });
                }
            }
        };

        push(
            "Camera make",
            self.string_tag(0x010f, ExifTagGroup::GENERIC),
        );
        push(
            "Camera model",
            self.string_tag(0x0110, ExifTagGroup::GENERIC),
        );
        push(
            "Date taken",
            self.string_tag(0x9003, ExifTagGroup::EXIF)
                .or_else(|| self.string_tag(0x0132, ExifTagGroup::GENERIC)),
        );
        push("Artist", self.string_tag(0x013b, ExifTagGroup::GENERIC));
        push("Copyright", self.string_tag(0x8298, ExifTagGroup::GENERIC));
        push("Software", self.string_tag(0x0131, ExifTagGroup::GENERIC));
        push(
            "Description",
            self.string_tag(0x010e, ExifTagGroup::GENERIC),
        );
        push("Orientation", Some(self.orientation().to_string()));

        if let Some(exposure) = self.rational_tag(0x829a, ExifTagGroup::EXIF, 0) {
            push(
                "Exposure time",
                Some(format!("1/{:.0} s", 1.0 / exposure.max(1e-9))),
            );
        }
        if let Some(f_number) = self.rational_tag(0x829d, ExifTagGroup::EXIF, 0) {
            push("Aperture", Some(format!("f/{f_number:.1}")));
        }
        if let Some(iso) = self.u16_tag(0x8827, ExifTagGroup::EXIF) {
            push("ISO", Some(iso.to_string()));
        }
        if let Some(focal) = self.rational_tag(0x920a, ExifTagGroup::EXIF, 0) {
            push("Focal length", Some(format!("{focal:.0} mm")));
        }
        if let Some(latitude) = self.gps_coordinate(0x0002, 0x0001) {
            push("GPS latitude", Some(latitude));
        }
        if let Some(longitude) = self.gps_coordinate(0x0004, 0x0003) {
            push("GPS longitude", Some(longitude));
        }

        entries
    }

    /// Removes every GPS tag and the GPS IFD pointer. GPS lives in its own
    /// IFD, so this is a targeted removal rather than wiping all metadata.
    pub fn strip_gps(&mut self) {
        let tags: Vec<ExifTag> = self
            .raw
            .get_ifds()
            .iter()
            .flat_map(|ifd| ifd.get_tags().iter().cloned())
            .filter(|tag| tag.get_group() == ExifTagGroup::GPS)
            .collect();
        for tag in tags {
            self.raw.remove_tag(tag);
        }
        self.raw
            .remove_tag_by_hex_group(0x8825, ExifTagGroup::GENERIC);
    }

    pub fn bake_orientation(&mut self) {
        self.raw.set_tag(ExifTag::Orientation(vec![1]));
    }

    /// Applies metadata to freshly encoded image bytes. Formats without an EXIF
    /// container simply keep their pixels.
    pub fn write_to(&self, encoded: &mut Vec<u8>) -> Result<()> {
        if self.raw.get_ifds().is_empty() {
            return Ok(());
        }
        self.raw
            .write_to_vec(encoded, self.file_type)
            .map_err(|error| AppError::Message(format!("could not write EXIF metadata: {error}")))
    }
}
