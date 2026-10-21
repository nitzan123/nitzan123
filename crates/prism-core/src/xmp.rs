//! XMP sidecars (`photo.NEF` → `photo.NEF.xmp`). Standard fields are used
//! where they exist so other tools (Lightroom, darktable, digiKam …) can read
//! ratings, labels and keywords; develop settings live in Prism's own
//! namespace as JSON.

use crate::develop::DevelopSettings;
use crate::error::Result;
use std::path::{Path, PathBuf};

pub const PRISM_NS: &str = "https://prism-studio.dev/ns/1.0/";

/// Colour label names written to `xmp:Label` (index = catalog value).
pub const LABELS: [&str; 6] = ["", "Red", "Yellow", "Green", "Blue", "Purple"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sidecar {
    pub rating: u8,
    pub color_label: u8,
    pub tagged: bool,
    pub caption: Option<String>,
    pub keywords: Vec<String>,
    pub develop: Option<DevelopSettings>,
}

pub fn sidecar_path(image: &Path) -> PathBuf {
    let mut s = image.as_os_str().to_owned();
    s.push(".xmp");
    PathBuf::from(s)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn unesc(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&gt;", ">").replace("&lt;", "<").replace("&amp;", "&")
}

pub fn to_xml(sc: &Sidecar) -> Result<String> {
    let mut attrs = format!("\n   xmp:Rating=\"{}\"", sc.rating.min(5));
    if let Some(l) = LABELS.get(sc.color_label as usize).filter(|l| !l.is_empty()) {
        attrs += &format!("\n   xmp:Label=\"{l}\"");
    }
    attrs += &format!("\n   prism:Tagged=\"{}\"", sc.tagged);
    let mut body = String::new();
    if !sc.keywords.is_empty() {
        body += "   <dc:subject>\n    <rdf:Bag>\n";
        for k in &sc.keywords {
            body += &format!("     <rdf:li>{}</rdf:li>\n", esc(k));
        }
        body += "    </rdf:Bag>\n   </dc:subject>\n";
    }
    if let Some(c) = sc.caption.as_ref().filter(|c| !c.is_empty()) {
        body += &format!(
            "   <dc:description>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </dc:description>\n",
            esc(c)
        );
    }
    if let Some(d) = sc.develop.as_ref().filter(|d| !d.is_identity()) {
        body += &format!("   <prism:Develop>{}</prism:Develop>\n", esc(&serde_json::to_string(d)?));
    }
    Ok(format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Prism Studio\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\"\n   xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n   xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n   xmlns:prism=\"{PRISM_NS}\"{attrs}>\n{body}  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>\n"
    ))
}

/// Minimal reader for the fields above. Tolerates sidecars written by other
/// tools (attribute or element form for rating/label).
pub fn from_xml(xml: &str) -> Sidecar {
    let attr_or_elem = |name: &str| -> Option<String> {
        if let Some(i) = xml.find(&format!("{name}=\"")) {
            let rest = &xml[i + name.len() + 2..];
            return rest.find('"').map(|j| unesc(&rest[..j]));
        }
        let open = format!("<{name}>");
        let i = xml.find(&open)?;
        let rest = &xml[i + open.len()..];
        rest.find(&format!("</{name}>")).map(|j| unesc(rest[..j].trim()))
    };
    let li_in = |container: &str| -> Vec<String> {
        let Some(i) = xml.find(&format!("<{container}>")) else { return Vec::new() };
        let Some(j) = xml[i..].find(&format!("</{container}>")) else { return Vec::new() };
        let block = &xml[i..i + j];
        block
            .split("<rdf:li")
            .skip(1)
            .filter_map(|s| {
                let s = &s[s.find('>')? + 1..];
                Some(unesc(s[..s.find("</rdf:li>")?].trim()))
            })
            .filter(|s| !s.is_empty())
            .collect()
    };
    Sidecar {
        rating: attr_or_elem("xmp:Rating").and_then(|r| r.parse::<i32>().ok()).unwrap_or(0).clamp(0, 5) as u8,
        color_label: attr_or_elem("xmp:Label")
            .and_then(|l| LABELS.iter().position(|x| !x.is_empty() && x.eq_ignore_ascii_case(&l)))
            .unwrap_or(0) as u8,
        tagged: attr_or_elem("prism:Tagged").is_some_and(|t| t == "true"),
        caption: li_in("dc:description").into_iter().next(),
        keywords: li_in("dc:subject"),
        develop: attr_or_elem("prism:Develop").and_then(|j| serde_json::from_str(&j).ok()),
    }
}

pub fn write_sidecar(image: &Path, sc: &Sidecar) -> Result<()> {
    let target = sidecar_path(image);
    let tmp = target.with_extension("xmp.tmp");
    std::fs::write(&tmp, to_xml(sc)?)?;
    std::fs::rename(tmp, target)?;
    Ok(())
}

pub fn read_sidecar(image: &Path) -> Option<Sidecar> {
    std::fs::read_to_string(sidecar_path(image)).ok().map(|x| from_xml(&x))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_fields() {
        let mut d = DevelopSettings::default();
        d.light.exposure = 0.7;
        d.mixer.hue[2] = -12.0;
        let sc = Sidecar {
            rating: 4,
            color_label: 3,
            tagged: true,
            caption: Some("Sunset <over> \"bay\" & hills".into()),
            keywords: vec!["beach".into(), "R&D".into()],
            develop: Some(d),
        };
        let xml = to_xml(&sc).unwrap();
        assert!(xml.contains("xmp:Rating=\"4\"") && xml.contains("xmp:Label=\"Green\""));
        assert_eq!(from_xml(&xml), sc);
    }

    #[test]
    fn reads_foreign_element_form() {
        let xml = r#"<rdf:Description><xmp:Rating>5</xmp:Rating><xmp:Label>Red</xmp:Label>
            <dc:subject><rdf:Bag><rdf:li>cat</rdf:li></rdf:Bag></dc:subject></rdf:Description>"#;
        let sc = from_xml(xml);
        assert_eq!((sc.rating, sc.color_label, sc.keywords.clone()), (5, 1, vec!["cat".to_string()]));
        assert!(sc.develop.is_none());
    }

    #[test]
    fn writes_next_to_image() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("x.NEF");
        write_sidecar(&img, &Sidecar { rating: 2, ..Default::default() }).unwrap();
        assert!(dir.path().join("x.NEF.xmp").exists());
        assert_eq!(read_sidecar(&img).unwrap().rating, 2);
    }
}
