//! Documents for `read_file`: what kind a file is, and text out of the formats a model cannot
//! read raw (PDF, Word, PowerPoint, spreadsheets). Images are passed to vision models as they are.

use anyhow::{bail, Context, Result};
use std::io::{Cursor, Read};

pub enum Kind {
    Text,
    Image(&'static str),
    /// The format name for an `input_audio` part.
    Audio(&'static str),
    Pdf,
    Docx,
    Pptx,
    Sheet,
}

/// The kind of a file, by its extension. Anything unknown is read as text.
pub fn kind(path: &str) -> Kind {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => Kind::Image("image/png"),
        "jpg" | "jpeg" => Kind::Image("image/jpeg"),
        "gif" => Kind::Image("image/gif"),
        "webp" => Kind::Image("image/webp"),
        "mp3" => Kind::Audio("mp3"),
        "wav" => Kind::Audio("wav"),
        "m4a" => Kind::Audio("m4a"),
        "ogg" | "oga" => Kind::Audio("ogg"),
        "flac" => Kind::Audio("flac"),
        "aac" => Kind::Audio("aac"),
        "pdf" => Kind::Pdf,
        "docx" => Kind::Docx,
        "pptx" => Kind::Pptx,
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => Kind::Sheet,
        _ => Kind::Text,
    }
}

/// The text of a PDF, Word, PowerPoint or spreadsheet file.
pub fn extract(kind: &Kind, bytes: &[u8]) -> Result<String> {
    match kind {
        Kind::Pdf => pdf(bytes),
        Kind::Docx => docx(bytes),
        Kind::Pptx => pptx(bytes),
        Kind::Sheet => sheets(bytes),
        Kind::Text | Kind::Image(_) | Kind::Audio(_) => bail!("not a document"),
    }
}

fn pdf(bytes: &[u8]) -> Result<String> {
    // pdf-extract separates pages with form feeds.
    let text = pdf_extract::extract_text_from_mem(bytes).context("could not read the PDF")?;
    let pages: Vec<String> = text.split('\u{c}').map(|p| p.trim().to_string()).collect();
    let n = pages.len();
    let mut out = String::new();
    for (i, p) in pages.iter().enumerate().filter(|(_, p)| !p.is_empty()) {
        out += &format!("--- page {} of {n} ---\n{p}\n\n", i + 1);
    }
    if out.trim().is_empty() {
        bail!("the PDF has no text layer (it is probably scanned images)");
    }
    Ok(out.trim_end().to_string())
}

fn sheets(bytes: &[u8]) -> Result<String> {
    use calamine::Reader;
    let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).context("could not read the spreadsheet")?;
    let mut out = String::new();
    for name in book.sheet_names() {
        let range = book.worksheet_range(&name).with_context(|| format!("sheet {name}"))?;
        out += &format!("--- sheet {name} ({} rows) ---\n", range.height());
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(|c| csv_cell(&c.to_string())).collect();
            out += &cells.join(",");
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out.trim_end().to_string())
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The text runs of one Office XML part: `text` elements joined, a newline after each paragraph.
fn xml_text(xml: &str, text: &str, para: &str) -> String {
    use quick_xml::events::Event;
    let mut r = quick_xml::Reader::from_str(xml);
    let (mut out, mut inside) = (String::new(), false);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == text => inside = true,
            Ok(Event::End(e)) if e.name().as_ref() == text => inside = false,
            Ok(Event::End(e)) if e.name().as_ref() == para => out.push('\n'),
            Ok(Event::Empty(e)) if e.name().as_ref() == "w:tab" => out.push('\t'),
            Ok(Event::Text(t)) if inside => out += &t.xml10_content(),
            Ok(Event::GeneralRef(g)) if inside => {
                // Entities such as &amp; arrive as their own events.
                let name: &str = g.as_ref();
                out += match name {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                };
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn zip_part(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<String> {
    let mut s = String::new();
    zip.by_name(name).with_context(|| format!("missing {name}"))?.read_to_string(&mut s)?;
    Ok(s)
}

fn docx(bytes: &[u8]) -> Result<String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("not a valid .docx file")?;
    let xml = zip_part(&mut zip, "word/document.xml")?;
    Ok(xml_text(&xml, "w:t", "w:p").trim().to_string())
}

fn pptx(bytes: &[u8]) -> Result<String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("not a valid .pptx file")?;
    let mut slides: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|n| n.ok())
        .filter_map(|n| n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok().map(|i| (i, n.to_string())))
        .collect();
    slides.sort();
    let mut out = String::new();
    for (i, name) in slides {
        let text = xml_text(&zip_part(&mut zip, &name)?, "a:t", "a:p");
        out += &format!("--- slide {i} ---\n{}\n\n", text.trim());
    }
    Ok(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn office(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, body) in parts {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn docx_keeps_paragraphs_tabs_and_entities() {
        let xml =
            r#"<w:document><w:body><w:p><w:r><w:t>Q4 &amp; Q3</w:t></w:r></w:p><w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r></w:p></w:body></w:document>"#;
        let out = extract(&Kind::Docx, &office(&[("word/document.xml", xml)])).unwrap();
        assert_eq!(out, "Q4 & Q3\na\tb");
    }

    #[test]
    fn pptx_orders_slides_numerically() {
        let slide = |t: &str| format!("<p:sld><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>");
        let (s2, s10) = (slide("second"), slide("tenth"));
        let out = extract(&Kind::Pptx, &office(&[("ppt/slides/slide10.xml", &s10), ("ppt/slides/slide2.xml", &s2)])).unwrap();
        assert_eq!(out, "--- slide 2 ---\nsecond\n\n--- slide 10 ---\ntenth");
    }

    #[test]
    fn kinds_by_extension() {
        assert!(matches!(kind("a/B.PNG"), Kind::Image("image/png")));
        assert!(matches!(kind("report.pdf"), Kind::Pdf));
        assert!(matches!(kind("data.xlsx"), Kind::Sheet));
        assert!(matches!(kind("notes"), Kind::Text));
    }
}
