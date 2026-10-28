use lopdf::{Document, Object, Stream, dictionary};

use crate::error::Result;

/// Wraps a raster image in a single-page PDF. Alpha is preserved with an SMask,
/// and the page is sized at 1 pixel = 1 point, matching Preview's image export.
pub fn image_to_pdf(image: &image::RgbaImage) -> Result<Vec<u8>> {
    let width = image.width();
    let height = image.height();
    let mut doc = Document::with_version("1.6");

    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    let mut alpha = Vec::with_capacity((width * height) as usize);
    let mut has_transparency = false;
    for pixel in image.pixels() {
        rgb.extend_from_slice(&pixel.0[0..3]);
        alpha.push(pixel.0[3]);
        if pixel.0[3] != 255 {
            has_transparency = true;
        }
    }

    let alpha_id = if has_transparency {
        let mut stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => width as i64,
                "Height" => height as i64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8i64,
            },
            alpha,
        );
        stream.compress()?;
        Some(doc.add_object(stream))
    } else {
        None
    };

    let mut image_dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => width as i64,
        "Height" => height as i64,
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8i64,
    };
    if let Some(alpha_id) = alpha_id {
        image_dict.set("SMask", alpha_id);
    }
    let mut image_stream = Stream::new(image_dict, rgb);
    image_stream.compress()?;
    let image_id = doc.add_object(image_stream);

    let content = format!(
        "q\n{} 0 0 {} 0 0 cm\n/Im0 Do\nQ\n",
        width as f32, height as f32
    );
    let mut content_stream = Stream::new(dictionary! {}, content.into_bytes());
    content_stream.compress()?;
    let content_id = doc.add_object(content_stream);

    let resources = dictionary! {
        "XObject" => dictionary! { "Im0" => image_id },
    };

    let pages_id = doc.new_object_id();
    let page = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width as f32),
            Object::Real(height as f32),
        ],
        "Resources" => resources,
        "Contents" => content_id,
    };
    let page_id = doc.add_object(page);
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1i64,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)?;
    Ok(bytes)
}
