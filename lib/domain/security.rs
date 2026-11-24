//! Layered upload security: magic-byte checks, bounded DOCX/XLSX inspection,
//! antivirus gating, and unsafe extracted text chunk filtering.
use crate::{
    core::{Error, Result},
    dal::scanner::{ScanVerdict, VirusScanner},
};

pub const MAX_ZIP_ENTRIES: usize = 2000;
pub const MAX_ZIP_UNCOMPRESSED_BYTES: u64 = 50_000_000;
pub const MAX_COMPRESSION_RATIO: u64 = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    Pdf,
    Docx,
    Xlsx,
    PlainText,
    Image,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChunk {
    pub source_ref: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilteredExtraction {
    pub chunks: Vec<TextChunk>,
    pub omitted_count: usize,
    pub notice: Option<String>,
}

fn is_executable_or_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(b"MZ")
        || bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(b"\xfe\xed\xfa\xce")
        || bytes.starts_with(b"\xfe\xed\xfa\xcf")
        || bytes.starts_with(b"\xca\xfe\xba\xbe")
        || bytes.starts_with(b"#!")
        || bytes.starts_with(b"7z\xbc\xaf'\x1c")
        || bytes.starts_with(b"Rar!")
        || bytes.starts_with(b"\x1f\x8b")
}

fn read_u16_le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn read_u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn inspect_office_zip(bytes: &[u8], required_entry: &str) -> Result<()> {
    if bytes.len() < 22 || !bytes.starts_with(b"PK\x03\x04") {
        return Err(Error::unprocessable(
            "invalid_file: Expected a valid Office ZIP container",
        ));
    }
    // Locate End of Central Directory record (PK\x05\x06) within the last 64 KiB + 22 bytes.
    let search_start = bytes.len().saturating_sub(65_557);
    let eocd_pos = bytes[search_start..]
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .map(|pos| search_start + pos)
        .ok_or_else(|| Error::unprocessable("invalid_file: Missing ZIP directory"))?;

    if eocd_pos + 22 > bytes.len() {
        return Err(Error::unprocessable(
            "invalid_file: Truncated ZIP directory",
        ));
    }
    let eocd = &bytes[eocd_pos..];
    let total_entries = read_u16_le(&eocd[10..12]) as usize;
    let cd_size = read_u32_le(&eocd[12..16]) as usize;
    let cd_offset = read_u32_le(&eocd[16..20]) as usize;

    if total_entries == 0 || total_entries > MAX_ZIP_ENTRIES {
        return Err(Error::unprocessable(
            "invalid_file: ZIP entry count outside allowed bounds",
        ));
    }
    if cd_offset.saturating_add(cd_size) > eocd_pos {
        return Err(Error::unprocessable("invalid_file: Corrupt ZIP offsets"));
    }

    let cd = &bytes[cd_offset..cd_offset + cd_size];
    let mut cursor = 0usize;
    let mut total_uncompressed: u64 = 0;
    let mut saw_content_types = false;
    let mut saw_required = false;

    for _ in 0..total_entries {
        if cursor + 46 > cd.len() || &cd[cursor..cursor + 4] != b"PK\x01\x02" {
            return Err(Error::unprocessable("invalid_file: Corrupt ZIP entry"));
        }
        let compressed = read_u32_le(&cd[cursor + 20..cursor + 24]) as u64;
        let uncompressed = read_u32_le(&cd[cursor + 24..cursor + 28]) as u64;
        let name_len = read_u16_le(&cd[cursor + 28..cursor + 30]) as usize;
        let extra_len = read_u16_le(&cd[cursor + 30..cursor + 32]) as usize;
        let comment_len = read_u16_le(&cd[cursor + 32..cursor + 34]) as usize;

        let name_end = cursor + 46 + name_len;
        let next = name_end + extra_len + comment_len;
        if next > cd.len() {
            return Err(Error::unprocessable("invalid_file: Truncated ZIP entry"));
        }
        let name = std::str::from_utf8(&cd[cursor + 46..name_end])
            .map_err(|_| Error::unprocessable("invalid_file: Non-UTF8 ZIP entry name"))?;
        let lower = name.to_ascii_lowercase();

        if lower.contains("..") || lower.starts_with('/') {
            return Err(Error::unprocessable("unsafe_file: Invalid ZIP entry path"));
        }
        if lower.contains("vbaproject.bin")
            || lower.ends_with(".exe")
            || lower.ends_with(".dll")
            || lower.ends_with(".bat")
            || lower.ends_with(".ps1")
            || lower.ends_with(".sh")
            || lower.ends_with(".js")
            || lower.ends_with(".vbs")
            || lower.ends_with(".zip")
            || lower.ends_with(".rar")
            || lower.ends_with(".7z")
        {
            return Err(Error::unprocessable(
                "unsafe_file: Macros, executables, and nested archives are forbidden",
            ));
        }
        if compressed > 0 && uncompressed / compressed > MAX_COMPRESSION_RATIO {
            return Err(Error::unprocessable(
                "unsafe_file: Excessive compression ratio rejected",
            ));
        }
        total_uncompressed = total_uncompressed.saturating_add(uncompressed);
        if total_uncompressed > MAX_ZIP_UNCOMPRESSED_BYTES {
            return Err(Error::unprocessable(
                "unsafe_file: Uncompressed Office size exceeds limit",
            ));
        }
        if name == "[Content_Types].xml" {
            saw_content_types = true;
        }
        if name == required_entry {
            saw_required = true;
        }
        cursor = next;
    }

    if !saw_content_types || !saw_required {
        return Err(Error::unprocessable(
            "invalid_file: Missing required Office document structure",
        ));
    }
    Ok(())
}

pub fn validate_file_bytes(declared_mime: &str, bytes: &[u8]) -> Result<FileKind> {
    if bytes.is_empty() {
        return Err(Error::unprocessable("invalid_file: Empty file"));
    }
    if is_executable_or_archive(bytes) {
        return Err(Error::unprocessable(
            "unsafe_file: Executables and generic archives are not allowed",
        ));
    }
    match declared_mime {
        "application/pdf" => {
            if !bytes.starts_with(b"%PDF-") {
                return Err(Error::unprocessable("invalid_file: Invalid PDF signature"));
            }
            Ok(FileKind::Pdf)
        }
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            inspect_office_zip(bytes, "word/document.xml")?;
            Ok(FileKind::Docx)
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            inspect_office_zip(bytes, "xl/workbook.xml")?;
            Ok(FileKind::Xlsx)
        }
        "text/plain" | "text/markdown" | "text/csv" => {
            if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
                return Err(Error::unprocessable(
                    "invalid_file: Text upload must be valid UTF-8 without NUL bytes",
                ));
            }
            Ok(FileKind::PlainText)
        }
        "image/png" => {
            if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(Error::unprocessable("invalid_file: Invalid PNG signature"));
            }
            Ok(FileKind::Image)
        }
        "image/jpeg" => {
            if !bytes.starts_with(b"\xff\xd8\xff") {
                return Err(Error::unprocessable("invalid_file: Invalid JPEG signature"));
            }
            Ok(FileKind::Image)
        }
        "image/webp" => {
            if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
                return Err(Error::unprocessable("invalid_file: Invalid WebP signature"));
            }
            Ok(FileKind::Image)
        }
        "audio/mpeg" | "audio/mp3" => {
            let valid_id3 = bytes.starts_with(b"ID3");
            let valid_frame = bytes.len() >= 2 && bytes[0] == 0xff && (bytes[1] & 0xe0) == 0xe0;
            if !valid_id3 && !valid_frame {
                return Err(Error::unprocessable("invalid_file: Invalid MP3 signature"));
            }
            Ok(FileKind::Audio)
        }
        "audio/wav" | "audio/x-wav" | "audio/wave" => {
            if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
                return Err(Error::unprocessable("invalid_file: Invalid WAV signature"));
            }
            Ok(FileKind::Audio)
        }
        "audio/ogg" => {
            if !bytes.starts_with(b"OggS") {
                return Err(Error::unprocessable("invalid_file: Invalid Ogg signature"));
            }
            Ok(FileKind::Audio)
        }
        "audio/flac" => {
            if !bytes.starts_with(b"fLaC") {
                return Err(Error::unprocessable("invalid_file: Invalid FLAC signature"));
            }
            Ok(FileKind::Audio)
        }
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => {
            if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
                return Err(Error::unprocessable(
                    "invalid_file: Invalid MP4/M4A signature",
                ));
            }
            Ok(FileKind::Audio)
        }
        "audio/webm" => {
            if !bytes.starts_with(b"\x1a\x45\xdf\xa3") {
                return Err(Error::unprocessable("invalid_file: Invalid WebM signature"));
            }
            Ok(FileKind::Audio)
        }
        _ => Err(Error::unprocessable(
            "unsupported_media_type: Unsupported file type",
        )),
    }
}

pub async fn scan_and_validate(
    scanner: &dyn VirusScanner,
    declared_mime: &str,
    bytes: &[u8],
) -> Result<FileKind> {
    let kind = validate_file_bytes(declared_mime, bytes)?;
    match scanner.scan_bytes(bytes).await {
        ScanVerdict::Clean => Ok(kind),
        ScanVerdict::Infected(_) => Err(Error::unprocessable(
            "malware_detected: File blocked by security scan",
        )),
        ScanVerdict::Unavailable(_) => Err(Error::unavailable()),
    }
}

fn is_unsafe_chunk(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains('\0')
        || lower.contains("<script")
        || lower.contains("javascript:")
        || lower.contains("ignore previous instructions")
        || lower.contains("system prompt:")
}

pub fn filter_extracted_chunks(chunks: Vec<TextChunk>) -> Result<FilteredExtraction> {
    let mut safe = Vec::with_capacity(chunks.len());
    let mut omitted = 0usize;
    for chunk in chunks {
        if is_unsafe_chunk(&chunk.text) {
            omitted += 1;
        } else if !chunk.text.trim().is_empty() {
            safe.push(chunk);
        }
    }
    if safe.is_empty() {
        return Err(Error::unprocessable(
            "no_usable_material: No safe usable text remained after security filtering",
        ));
    }
    let notice = if omitted > 0 {
        Some(format!(
            "{omitted} unsafe extracted text chunk(s) excluded; original source preserved"
        ))
    } else {
        None
    };
    Ok(FilteredExtraction {
        chunks: safe,
        omitted_count: omitted,
        notice,
    })
}
