//! What is on this machine's clipboard, for Lua's `app:clipboard`: each
//! kind in one form a program elsewhere can use. Reading a picture and
//! making a PNG of it is slow, so this is only ever called on a worker.

/// The kinds Lua asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Kinds {
    pub(crate) text: bool,
    pub(crate) image: bool,
    pub(crate) files: bool,
}

impl Kinds {
    /// The kinds named, as Lua names them.
    pub(crate) fn of(names: &[String]) -> Result<Self, String> {
        let mut kinds = Self::default();
        for name in names {
            match name.as_str() {
                "text" => kinds.text = true,
                "image" => kinds.image = true,
                "files" => kinds.files = true,
                other => return Err(format!("{other:?} is not a kind of clipboard content: text, image or files")),
            }
        }
        Ok(kinds)
    }
}

/// What was found of the kinds asked for. A kind the clipboard does not
/// hold is `None`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Clip {
    pub(crate) text: Option<String>,
    /// The bytes of a PNG file.
    pub(crate) image: Option<Vec<u8>>,
    /// Full paths, on this machine.
    pub(crate) files: Option<Vec<String>>,
}

/// Read the clipboard for `kinds`. An error only when it cannot be read at
/// all: another program holds it, say.
pub(crate) fn read(kinds: Kinds) -> Result<Clip, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|error| format!("the clipboard could not be opened: {error}"))?;
    let mut clip = Clip::default();
    if kinds.text {
        clip.text = clipboard.get_text().ok();
    }
    if kinds.files {
        clip.files = clipboard
            .get()
            .file_list()
            .ok()
            .map(|files| files.into_iter().map(|path| path.to_string_lossy().into_owned()).collect())
            .filter(|files: &Vec<String>| !files.is_empty());
    }
    if kinds.image {
        // Kept apart from arboard's: the clipboard is opened by one at a time.
        drop(clipboard);
        clip.image = match png_as_it_is() {
            Some(png) => Some(png),
            None => {
                let mut clipboard = arboard::Clipboard::new()
                    .map_err(|error| format!("the clipboard could not be opened: {error}"))?;
                match clipboard.get_image() {
                    Ok(image) => Some(encode(image.width, image.height, image.bytes.into_owned())?),
                    Err(_) => None,
                }
            }
        };
    }
    Ok(clip)
}

/// The PNG file a program put on the clipboard itself, which browsers and
/// many others do beside the bitmap. arboard would decode it to pixels for
/// them to be encoded again.
#[cfg(windows)]
fn png_as_it_is() -> Option<Vec<u8>> {
    let format = clipboard_win::register_format("PNG")?.get();
    let _open = clipboard_win::Clipboard::new_attempts(10).ok()?;
    if !clipboard_win::is_format_avail(format) {
        return None;
    }
    let mut bytes = Vec::new();
    clipboard_win::raw::get_vec(format, &mut bytes).ok()?;
    bytes.starts_with(b"\x89PNG\r\n\x1a\n").then_some(bytes)
}

#[cfg(not(windows))]
fn png_as_it_is() -> Option<Vec<u8>> {
    None
}

/// A PNG of RGBA pixels.
fn encode(width: usize, height: usize, mut rgba: Vec<u8>) -> Result<Vec<u8>, String> {
    let failed = |error: png::EncodingError| format!("the picture could not be made a PNG: {error}");
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err("the picture is too large".into());
    };
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("the clipboard's picture is not whole".into());
    }
    // Some programs leave every pixel's alpha at nothing, which would be a
    // picture nobody can see. No picture meant to be seen is all clear.
    if rgba.chunks_exact(4).all(|pixel| pixel[3] == 0) {
        rgba.chunks_exact_mut(4).for_each(|pixel| pixel[3] = 255);
    }
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // A screenshot is sent at once: quick to make matters more than small.
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(failed)?;
    writer.write_image_data(&rgba).map_err(failed)?;
    writer.finish().map_err(failed)?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        pixels.truncate(info.buffer_size());
        (info.width, info.height, pixels)
    }

    #[test]
    fn pixels_become_a_png_of_the_same_pixels() {
        let rgba: Vec<u8> = (0..3 * 2 * 4).map(|n| (n * 13) as u8).collect();
        let png = encode(3, 2, rgba.clone()).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert_eq!(decode(&png), (3, 2, rgba));
        assert!(encode(3, 2, vec![0; 5]).is_err());
    }

    #[test]
    fn a_picture_with_no_alpha_at_all_is_shown() {
        let rgba = vec![10, 20, 30, 0, 40, 50, 60, 0];
        let (_, _, pixels) = decode(&encode(2, 1, rgba).unwrap());
        assert_eq!(pixels, vec![10, 20, 30, 255, 40, 50, 60, 255]);
        // One clear pixel among others is as it was.
        let rgba = vec![10, 20, 30, 0, 40, 50, 60, 9];
        let (_, _, pixels) = decode(&encode(2, 1, rgba.clone()).unwrap());
        assert_eq!(pixels, rgba);
    }

    #[test]
    fn kinds_are_named_as_lua_names_them() {
        let names = |names: &[&str]| Kinds::of(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>());
        assert_eq!(names(&["image", "files"]), Ok(Kinds { text: false, image: true, files: true }));
        assert_eq!(names(&[]), Ok(Kinds::default()));
        assert!(names(&["html"]).is_err());
    }
}
