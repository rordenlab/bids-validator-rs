//! Bounded TIFF / OME-TIFF metadata parser for `rules.checks.micr.*`.

use std::io::{Read, Seek, SeekFrom};

const IMAGE_DESCRIPTION_TAG: u16 = 0x010e;
const ASCII_TYPE: u16 = 2;
const MAX_IFD_ENTRIES: u64 = 4096;
const MAX_IFD_CHAIN: usize = 16;
const MAX_IMAGE_DESCRIPTION_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct TiffMetadata {
    pub version: u16,
}

#[derive(Debug, Clone)]
pub struct OmeMetadata {
    pub physical_size_x: f64,
    pub physical_size_y: f64,
    pub physical_size_z: f64,
    pub physical_size_x_unit: String,
    pub physical_size_y_unit: String,
    pub physical_size_z_unit: String,
}

#[derive(Debug, Clone, Copy)]
enum IfdLayout {
    Classic,
    BigTiff,
}

pub fn parse_tiff<R: Read + Seek>(
    mut reader: R,
    parse_ome: bool,
) -> (Option<TiffMetadata>, Option<OmeMetadata>) {
    let header = read_prefix(&mut reader, 16);
    if header.len() < 8 {
        return (None, None);
    }
    let little = match &header[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return (None, None),
    };
    let version = read_u16(&header[2..4], little);
    let tiff = Some(TiffMetadata { version });
    if !parse_ome {
        return (tiff, None);
    }

    let Some((layout, first_ifd_offset)) = ifd_layout_and_offset(&header, little, version) else {
        return (tiff, None);
    };
    let Some(desc) = image_description(&mut reader, little, layout, first_ifd_offset) else {
        return (tiff, None);
    };

    (tiff, parse_ome_xml(&desc))
}

fn ifd_layout_and_offset(header: &[u8], little: bool, version: u16) -> Option<(IfdLayout, u64)> {
    match version {
        42 => Some((
            IfdLayout::Classic,
            read_u32(header.get(4..8)?, little) as u64,
        )),
        43 => {
            // Spec BigTIFF stores offset-size and zero fields before the
            // first 64-bit IFD offset. Some tiny microscopy fixtures use
            // magic 43 with a classic 32-bit IFD layout; accept that as a
            // compatibility fallback.
            if header.len() >= 16
                && read_u16(header.get(4..6)?, little) == 8
                && read_u16(header.get(6..8)?, little) == 0
            {
                Some((IfdLayout::BigTiff, read_u64(header.get(8..16)?, little)))
            } else {
                Some((
                    IfdLayout::Classic,
                    read_u32(header.get(4..8)?, little) as u64,
                ))
            }
        }
        _ => None,
    }
}

fn image_description<R: Read + Seek>(
    reader: &mut R,
    little: bool,
    layout: IfdLayout,
    first_ifd_offset: u64,
) -> Option<String> {
    let mut ifd_offset = first_ifd_offset;
    let mut seen = Vec::new();
    for _ in 0..MAX_IFD_CHAIN {
        if ifd_offset == 0 || seen.contains(&ifd_offset) {
            return None;
        }
        seen.push(ifd_offset);

        let entry_count = read_ifd_count(reader, ifd_offset, little, layout)?;
        if entry_count > MAX_IFD_ENTRIES {
            return None;
        }
        let entries_start = ifd_offset.checked_add(layout.count_bytes())?;
        for i in 0..entry_count {
            let entry_offset = entries_start.checked_add(i.checked_mul(layout.entry_bytes())?)?;
            let entry = read_at(reader, entry_offset, layout.entry_bytes() as usize)?;
            let tag = read_u16(entry.get(0..2)?, little);
            if tag != IMAGE_DESCRIPTION_TAG {
                continue;
            }

            let value_type = read_u16(entry.get(2..4)?, little);
            if value_type != ASCII_TYPE {
                return None;
            }
            let nbytes = layout.read_value_count(&entry, little)?;
            if nbytes == 0 || nbytes > MAX_IMAGE_DESCRIPTION_BYTES {
                return None;
            }
            let mut raw = if nbytes <= layout.inline_value_bytes() {
                let value_start = layout.value_offset_start();
                let value_end = value_start.checked_add(usize::try_from(nbytes).ok()?)?;
                entry.get(value_start..value_end)?.to_vec()
            } else {
                let value_offset = layout.read_value_offset(&entry, little)?;
                read_at(reader, value_offset, usize::try_from(nbytes).ok()?)?
            };
            while raw.last() == Some(&0) {
                raw.pop();
            }
            return String::from_utf8(raw).ok();
        }

        ifd_offset = read_next_ifd_offset(reader, entries_start, entry_count, little, layout)?;
    }
    None
}

impl IfdLayout {
    fn count_bytes(self) -> u64 {
        match self {
            Self::Classic => 2,
            Self::BigTiff => 8,
        }
    }

    fn entry_bytes(self) -> u64 {
        match self {
            Self::Classic => 12,
            Self::BigTiff => 20,
        }
    }

    fn next_offset_bytes(self) -> u64 {
        match self {
            Self::Classic => 4,
            Self::BigTiff => 8,
        }
    }

    fn inline_value_bytes(self) -> u64 {
        match self {
            Self::Classic => 4,
            Self::BigTiff => 8,
        }
    }

    fn value_offset_start(self) -> usize {
        match self {
            Self::Classic => 8,
            Self::BigTiff => 12,
        }
    }

    fn read_value_count(self, entry: &[u8], little: bool) -> Option<u64> {
        match self {
            Self::Classic => Some(read_u32(entry.get(4..8)?, little) as u64),
            Self::BigTiff => Some(read_u64(entry.get(4..12)?, little)),
        }
    }

    fn read_value_offset(self, entry: &[u8], little: bool) -> Option<u64> {
        match self {
            Self::Classic => Some(read_u32(entry.get(8..12)?, little) as u64),
            Self::BigTiff => Some(read_u64(entry.get(12..20)?, little)),
        }
    }
}

fn read_ifd_count<R: Read + Seek>(
    reader: &mut R,
    ifd_offset: u64,
    little: bool,
    layout: IfdLayout,
) -> Option<u64> {
    let raw = read_at(reader, ifd_offset, layout.count_bytes() as usize)?;
    match layout {
        IfdLayout::Classic => Some(read_u16(&raw, little) as u64),
        IfdLayout::BigTiff => Some(read_u64(&raw, little)),
    }
}

fn read_next_ifd_offset<R: Read + Seek>(
    reader: &mut R,
    entries_start: u64,
    entry_count: u64,
    little: bool,
    layout: IfdLayout,
) -> Option<u64> {
    let next_offset = entries_start.checked_add(entry_count.checked_mul(layout.entry_bytes())?)?;
    let raw = read_at(reader, next_offset, layout.next_offset_bytes() as usize)?;
    match layout {
        IfdLayout::Classic => Some(read_u32(&raw, little) as u64),
        IfdLayout::BigTiff => Some(read_u64(&raw, little)),
    }
}

fn read_at<R: Read + Seek>(reader: &mut R, offset: u64, len: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; len];
    reader.seek(SeekFrom::Start(offset)).ok()?;
    reader.read_exact(&mut out).ok()?;
    Some(out)
}

fn read_prefix<R: Read + Seek>(reader: &mut R, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    if reader.seek(SeekFrom::Start(0)).is_err() {
        return Vec::new();
    }
    let mut filled = 0;
    while filled < len {
        match reader.read(&mut out[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return Vec::new(),
        }
    }
    out.truncate(filled);
    out
}

fn parse_ome_xml(xml: &str) -> Option<OmeMetadata> {
    let tag = find_start_tag(xml, "Pixels")?;
    Some(OmeMetadata {
        physical_size_x: attr(tag, "PhysicalSizeX")?.parse().ok()?,
        physical_size_y: attr(tag, "PhysicalSizeY")?.parse().ok()?,
        physical_size_z: attr(tag, "PhysicalSizeZ")?.parse().ok()?,
        physical_size_x_unit: attr(tag, "PhysicalSizeXUnit")?,
        physical_size_y_unit: attr(tag, "PhysicalSizeYUnit")?,
        physical_size_z_unit: attr(tag, "PhysicalSizeZUnit")?,
    })
}

fn find_start_tag<'a>(xml: &'a str, wanted_local_name: &str) -> Option<&'a str> {
    let mut pos = 0;
    while let Some(rel) = xml.get(pos..)?.find('<') {
        let start = pos.checked_add(rel)?;
        let tail = xml.get(start..)?;
        if tail.starts_with("<!--") {
            pos = start.checked_add(tail.find("-->")?)?.checked_add(3)?;
            continue;
        }
        if tail.starts_with("<?") {
            pos = start.checked_add(tail.find("?>")?)?.checked_add(2)?;
            continue;
        }
        if tail.starts_with("</") || tail.starts_with("<!") {
            pos = find_tag_end(xml, start.checked_add(1)?)?.checked_add(1)?;
            continue;
        }

        let name_start = start.checked_add(1)?;
        let name_end = xml
            .get(name_start..)?
            .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')?
            .checked_add(name_start)?;
        if name_end == name_start {
            pos = start.checked_add(1)?;
            continue;
        }
        let tag_end = find_tag_end(xml, name_end)?;
        let name = xml.get(name_start..name_end)?;
        if local_name(name) == wanted_local_name {
            return xml.get(name_start..tag_end);
        }
        pos = tag_end.checked_add(1)?;
    }
    None
}

fn find_tag_end(xml: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    for (rel, c) in xml.get(from..)?.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '>' => return from.checked_add(rel),
            None => {}
        }
    }
    None
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let mut i = tag
        .find(|c: char| c.is_ascii_whitespace() || c == '/')
        .unwrap_or(tag.len());
    while i < tag.len() {
        i = skip_ascii_ws(tag, i);
        if i >= tag.len() || tag.as_bytes()[i] == b'/' {
            return None;
        }

        let attr_start = i;
        while i < tag.len() {
            let b = tag.as_bytes()[i];
            if b.is_ascii_whitespace() || b == b'=' || b == b'/' {
                break;
            }
            i += 1;
        }
        let attr_name = tag.get(attr_start..i)?;
        i = skip_ascii_ws(tag, i);
        if i >= tag.len() || tag.as_bytes()[i] != b'=' {
            return None;
        }
        i += 1;
        i = skip_ascii_ws(tag, i);
        if i >= tag.len() {
            return None;
        }
        let quote = tag.as_bytes()[i];
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        i += 1;
        let value_start = i;
        while i < tag.len() && tag.as_bytes()[i] != quote {
            i += 1;
        }
        let value = tag.get(value_start..i)?;
        if i >= tag.len() {
            return None;
        }
        i += 1;

        if local_name(attr_name) == name {
            return decode_xml_entities(value);
        }
    }
    None
}

fn local_name(name: &str) -> &str {
    name.rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(name)
}

fn skip_ascii_ws(s: &str, mut i: usize) -> usize {
    while i < s.len() && s.as_bytes()[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn decode_xml_entities(value: &str) -> Option<String> {
    if !value.contains('&') {
        return Some(value.to_string());
    }

    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        out.push_str(rest.get(..start)?);
        let after_amp = rest.get(start.checked_add(1)?..)?;
        let end = after_amp.find(';')?;
        let entity = after_amp.get(..end)?;
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                let code = u32::from_str_radix(entity.get(2..)?, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            _ if entity.starts_with('#') => {
                let code = entity.get(1..)?.parse::<u32>().ok()?;
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
        rest = after_amp.get(end.checked_add(1)?..)?;
    }
    out.push_str(rest);
    Some(out)
}

fn read_u16(bytes: &[u8], little: bool) -> u16 {
    let arr = [bytes[0], bytes[1]];
    if little {
        u16::from_le_bytes(arr)
    } else {
        u16::from_be_bytes(arr)
    }
}

fn read_u32(bytes: &[u8], little: bool) -> u32 {
    let arr = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if little {
        u32::from_le_bytes(arr)
    } else {
        u32::from_be_bytes(arr)
    }
}

fn read_u64(bytes: &[u8], little: bool) -> u64 {
    let arr = [
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ];
    if little {
        u64::from_le_bytes(arr)
    } else {
        u64::from_be_bytes(arr)
    }
}

pub fn tiff_to_json(tiff: &TiffMetadata) -> serde_json::Value {
    serde_json::json!({ "version": tiff.version })
}

pub fn ome_to_json(ome: &OmeMetadata) -> serde_json::Value {
    serde_json::json!({
        "PhysicalSizeX": ome.physical_size_x,
        "PhysicalSizeY": ome.physical_size_y,
        "PhysicalSizeZ": ome.physical_size_z,
        "PhysicalSizeXUnit": ome.physical_size_x_unit,
        "PhysicalSizeYUnit": ome.physical_size_y_unit,
        "PhysicalSizeZUnit": ome.physical_size_z_unit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Cursor;
    use std::path::PathBuf;

    #[test]
    fn parses_tiff_header_from_reader() {
        let bytes = [b'I', b'I', 42, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let (tiff, ome) = parse_tiff(Cursor::new(bytes), true);
        assert_eq!(tiff.unwrap().version, 42);
        assert!(ome.is_none());
    }

    #[test]
    fn parses_classic_ome_tiff_beyond_initial_prefix() {
        let xml = ome_xml("um");
        let bytes = classic_tiff_with_description(5000, xml.as_bytes());
        let (tiff, ome) = parse_tiff(Cursor::new(bytes), true);
        assert_eq!(tiff.unwrap().version, 42);
        let ome = ome.unwrap();
        assert_eq!(ome.physical_size_x, 1.0);
        assert_eq!(ome.physical_size_y, 2.0);
        assert_eq!(ome.physical_size_z, 3.0);
        assert_eq!(ome.physical_size_x_unit, "um");
    }

    #[test]
    fn parses_spec_bigtiff_ome_description() {
        let xml = ome_xml("&#xB5;m");
        let bytes = bigtiff_with_description(128, xml.as_bytes());
        let (tiff, ome) = parse_tiff(Cursor::new(bytes), true);
        assert_eq!(tiff.unwrap().version, 43);
        let ome = ome.unwrap();
        assert_eq!(ome.physical_size_x_unit, "\u{00b5}m");
        assert_eq!(ome.physical_size_y_unit, "\u{00b5}m");
        assert_eq!(ome.physical_size_z_unit, "\u{00b5}m");
    }

    #[test]
    fn parses_existing_legacy_bigtiff_fixture() {
        let Some(path) = upstream_fixture("tests/data/ome-tiff/btif_id.ome.tif") else {
            eprintln!("skipping upstream OME-TIFF fixture; run scripts/fetch_upstream.py first");
            return;
        };
        let file = File::open(path).unwrap();
        let (tiff, ome) = parse_tiff(file, true);
        assert_eq!(tiff.unwrap().version, 43);
        let ome = ome.unwrap();
        assert_eq!(ome.physical_size_x, 1.0);
        assert_eq!(ome.physical_size_x_unit, "\u{00b5}m");
    }

    fn upstream_fixture(relative: &str) -> Option<PathBuf> {
        let upstream = std::env::var_os("BIDS_VALIDATOR_UPSTREAM_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bids-validator-2.4.1")
            });
        let path = upstream.join(relative);
        path.is_file().then_some(path)
    }

    #[test]
    fn malformed_tiff_does_not_panic_or_load_huge_metadata() {
        let mut bytes = classic_tiff_with_description(128, b"ignored");
        put_u16(&mut bytes, 8, MAX_IFD_ENTRIES as u16 + 1, true);
        let (tiff, ome) = parse_tiff(Cursor::new(bytes), true);
        assert_eq!(tiff.unwrap().version, 42);
        assert!(ome.is_none());
    }

    #[test]
    fn oversized_image_description_is_ignored() {
        let mut bytes = classic_tiff_with_description(128, b"ignored");
        put_u32(
            &mut bytes,
            14,
            u32::try_from(MAX_IMAGE_DESCRIPTION_BYTES + 1).unwrap(),
            true,
        );
        let (tiff, ome) = parse_tiff(Cursor::new(bytes), true);
        assert_eq!(tiff.unwrap().version, 42);
        assert!(ome.is_none());
    }

    #[test]
    fn malformed_ome_xml_is_ignored() {
        let bytes = classic_tiff_with_description(
            128,
            br#"<OME><Image><Pixels PhysicalSizeX="1" PhysicalSizeY="2""#,
        );
        let (tiff, ome) = parse_tiff(Cursor::new(bytes), true);
        assert_eq!(tiff.unwrap().version, 42);
        assert!(ome.is_none());
    }

    #[test]
    fn parses_namespaced_pixels_and_single_quoted_attributes() {
        let xml = concat!(
            "<ome:OME><ome:Image><ome:Pixels ",
            "PhysicalSizeX='1' PhysicalSizeY='2' PhysicalSizeZ='3' ",
            "PhysicalSizeXUnit='mm' PhysicalSizeYUnit='mm' PhysicalSizeZUnit='mm'/>",
            "</ome:Image></ome:OME>"
        );
        let bytes = classic_tiff_with_description(128, xml.as_bytes());
        let (_tiff, ome) = parse_tiff(Cursor::new(bytes), true);
        assert_eq!(ome.unwrap().physical_size_z_unit, "mm");
    }

    fn ome_xml(unit: &str) -> String {
        format!(
            concat!(
                "<?xml version=\"1.0\"?><OME><Image><Pixels ",
                "PhysicalSizeX=\"1\" PhysicalSizeY=\"2\" PhysicalSizeZ=\"3\" ",
                "PhysicalSizeXUnit=\"{unit}\" PhysicalSizeYUnit=\"{unit}\" ",
                "PhysicalSizeZUnit=\"{unit}\"/></Image></OME>"
            ),
            unit = unit
        )
    }

    fn classic_tiff_with_description(desc_offset: usize, desc: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; desc_offset + desc.len()];
        bytes[0] = b'I';
        bytes[1] = b'I';
        put_u16(&mut bytes, 2, 42, true);
        put_u32(&mut bytes, 4, 8, true);
        put_u16(&mut bytes, 8, 1, true);
        put_u16(&mut bytes, 10, IMAGE_DESCRIPTION_TAG, true);
        put_u16(&mut bytes, 12, ASCII_TYPE, true);
        put_u32(&mut bytes, 14, u32::try_from(desc.len()).unwrap(), true);
        put_u32(&mut bytes, 18, u32::try_from(desc_offset).unwrap(), true);
        put_u32(&mut bytes, 22, 0, true);
        bytes[desc_offset..desc_offset + desc.len()].copy_from_slice(desc);
        bytes
    }

    fn bigtiff_with_description(desc_offset: usize, desc: &[u8]) -> Vec<u8> {
        let ifd_offset = 32usize;
        let mut bytes = vec![0u8; desc_offset + desc.len()];
        bytes[0] = b'I';
        bytes[1] = b'I';
        put_u16(&mut bytes, 2, 43, true);
        put_u16(&mut bytes, 4, 8, true);
        put_u16(&mut bytes, 6, 0, true);
        put_u64(&mut bytes, 8, ifd_offset as u64, true);
        put_u64(&mut bytes, ifd_offset, 1, true);
        let entry = ifd_offset + 8;
        put_u16(&mut bytes, entry, IMAGE_DESCRIPTION_TAG, true);
        put_u16(&mut bytes, entry + 2, ASCII_TYPE, true);
        put_u64(&mut bytes, entry + 4, desc.len() as u64, true);
        put_u64(&mut bytes, entry + 12, desc_offset as u64, true);
        put_u64(&mut bytes, entry + 20, 0, true);
        bytes[desc_offset..desc_offset + desc.len()].copy_from_slice(desc);
        bytes
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16, little: bool) {
        let raw = if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        bytes[offset..offset + 2].copy_from_slice(&raw);
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32, little: bool) {
        let raw = if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        bytes[offset..offset + 4].copy_from_slice(&raw);
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64, little: bool) {
        let raw = if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        bytes[offset..offset + 8].copy_from_slice(&raw);
    }
}
