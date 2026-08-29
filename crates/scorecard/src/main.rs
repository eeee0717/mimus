use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u32 = 1;

#[derive(Parser)]
#[command(about = "Measure mimus output quality from public artifacts")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Measure(MeasureArgs),
}

#[derive(clap::Args)]
struct MeasureArgs {
    #[arg(long)]
    ndjson: PathBuf,
    #[arg(long)]
    debug_dir: PathBuf,
    #[arg(long)]
    input_pdf: PathBuf,
    #[arg(long)]
    output_pdf: PathBuf,
    #[arg(long)]
    json_out: PathBuf,
    #[arg(long)]
    markdown_out: PathBuf,
    #[arg(long, default_value_t = 72)]
    render_dpi: u32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct Rect {
    left: f64,
    bottom: f64,
    right: f64,
    top: f64,
}

#[derive(Debug, Deserialize)]
struct Il {
    pages: Vec<Page>,
}

#[derive(Debug, Deserialize)]
struct Page {
    index: usize,
    paragraphs: Vec<Paragraph>,
}

#[derive(Debug, Deserialize)]
struct Paragraph {
    reading_order: usize,
    bounds: Rect,
    text: Text,
    #[serde(default)]
    translated_text: Option<String>,
    #[serde(default)]
    preserved: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct Text {
    chars: Vec<Char>,
}

#[derive(Debug, Deserialize)]
struct Char {
    unicode: Option<String>,
    font_size: f64,
    baseline_origin: Point,
    #[serde(default)]
    layout: Option<Layout>,
}

#[derive(Debug, Deserialize)]
struct Point {
    y: f64,
}

#[derive(Debug, Deserialize)]
struct Layout {
    policy: String,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    input_sha256: String,
    output_sha256: String,
    output_characters: usize,
    dimensions: BTreeMap<String, Dimension>,
    total_score: f64,
    evidence: Evidence,
}

#[derive(Debug, Serialize)]
struct Dimension {
    formula_ids: Vec<&'static str>,
    weighted_errors: f64,
    errors_per_1000_output_characters: f64,
    score: f64,
    measurements: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct Evidence {
    qpdf_input_ok: bool,
    qpdf_output_ok: bool,
    input_is_output_prefix: bool,
    page_count_equal: bool,
    non_text_object_counts_equal: bool,
    non_text_pixel_fidelity: Option<f64>,
}

fn main() -> Result<()> {
    let Cli { command } = Cli::parse();
    match command {
        Commands::Measure(args) => measure(args),
    }
}

fn measure(args: MeasureArgs) -> Result<()> {
    let before: Il = read_json(args.debug_dir.join("03-paragraph_find.il.json"))?;
    let translated: Il = read_json(args.debug_dir.join("06-translate.il.json"))?;
    let typeset: Il = read_json(args.debug_dir.join("07-typeset.il.json"))?;
    let write: Il = read_json(args.debug_dir.join("09-write.il.json"))?;
    let events = read_ndjson(&args.ndjson)?;
    let output_chars = extracted_character_count(&args.output_pdf)?;
    let denominator = output_chars.max(1) as f64;

    let mut dimensions = BTreeMap::new();
    dimensions.insert(
        "coverage_gap".into(),
        coverage(&before, &translated, &typeset, denominator),
    );
    dimensions.insert(
        "overtranslation".into(),
        overtranslation(&before, &translated, denominator),
    );
    dimensions.insert(
        "mistranslation_risk".into(),
        risk(&before, &events, denominator),
    );
    dimensions.insert(
        "layout_drift".into(),
        layout(&before, &typeset, &events, denominator),
    );
    dimensions.insert(
        "typesetting_lint".into(),
        lint(&translated, &events, denominator),
    );

    let input_bytes = fs::read(&args.input_pdf)?;
    let output_bytes = fs::read(&args.output_pdf)?;
    let qpdf_input_ok = qpdf_check(&args.input_pdf);
    let qpdf_output_ok = qpdf_check(&args.output_pdf);
    let input_pages = qpdf_pages(&args.input_pdf);
    let output_pages = qpdf_pages(&args.output_pdf);
    let input_counts = qpdf_non_text_counts(&args.input_pdf)?;
    let output_counts = qpdf_non_text_counts(&args.output_pdf)?;
    let masks = translated_masks(&translated);
    let pixel_fidelity = Some(pixel_fidelity(
        &args.input_pdf,
        &args.output_pdf,
        &masks,
        args.render_dpi,
    )?);
    let evidence = Evidence {
        qpdf_input_ok,
        qpdf_output_ok,
        input_is_output_prefix: output_bytes.starts_with(&input_bytes),
        page_count_equal: input_pages.is_some() && input_pages == output_pages,
        non_text_object_counts_equal: input_counts == output_counts,
        non_text_pixel_fidelity: pixel_fidelity,
    };
    dimensions.insert(
        "structural_fidelity".into(),
        structure(&evidence, denominator),
    );
    let total_score =
        round6(dimensions.values().map(|d| d.score).sum::<f64>() / dimensions.len() as f64);
    let report = Report {
        schema_version: SCHEMA_VERSION,
        input_sha256: sha256(&input_bytes),
        output_sha256: sha256(&output_bytes),
        output_characters: output_chars,
        dimensions,
        total_score,
        evidence,
    };
    let json = serde_json::to_string_pretty(&report)? + "\n";
    write_atomic(&args.json_out, json.as_bytes())?;
    write_atomic(&args.markdown_out, markdown(&report).as_bytes())?;
    let _ = write.pages.len();
    Ok(())
}

fn coverage(before: &Il, translated: &Il, typeset: &Il, denominator: f64) -> Dimension {
    let mut eligible = 0usize;
    let mut covered = 0usize;
    let mut translated_han = 0usize;
    let mut published_han = 0usize;
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for page in &before.pages {
        for source in &page.paragraphs {
            if !is_translatable(source) {
                continue;
            }
            eligible += 1;
            let translated_paragraph = find_paragraph(translated, page.index, source.reading_order);
            let typeset_paragraph = find_paragraph(typeset, page.index, source.reading_order);
            translated_han += translated_paragraph
                .and_then(|p| p.translated_text.as_deref())
                .map(count_han)
                .unwrap_or(0);
            if translated_paragraph
                .is_some_and(|p| p.translated_text.is_some() && p.preserved.is_none())
                && typeset_paragraph
                    .is_some_and(|p| p.translated_text.is_some() && p.preserved.is_none())
            {
                covered += 1;
                published_han += typeset_paragraph
                    .and_then(|p| p.translated_text.as_deref())
                    .map(count_han)
                    .unwrap_or(0);
            } else if let Some(reason) = typeset_paragraph
                .and_then(preserved_reason)
                .or_else(|| translated_paragraph.and_then(preserved_reason))
            {
                *reasons.entry(reason).or_default() += 1;
            }
        }
    }
    let missing = eligible.saturating_sub(covered);
    let weighted = missing as f64 * 5.0;
    dimension(
        &["COV-01", "COV-02", "COV-03"],
        weighted,
        denominator,
        BTreeMap::from([
            ("eligible_paragraphs".into(), eligible.into()),
            ("translated_paragraphs".into(), covered.into()),
            (
                "paragraph_coverage".into(),
                ratio(covered as f64, eligible as f64).into(),
            ),
            (
                "han_weighted_coverage".into(),
                ratio(published_han as f64, translated_han as f64).into(),
            ),
            ("translated_han".into(), translated_han.into()),
            ("published_han".into(), published_han.into()),
            (
                "preserved_reasons".into(),
                serde_json::to_value(reasons).unwrap(),
            ),
        ]),
    )
}

fn overtranslation(before: &Il, after: &Il, denominator: f64) -> Dimension {
    let mut policy = 0usize;
    let mut superscript = 0usize;
    let mut numbering = 0usize;
    let mut blank = 0usize;
    for (a, b) in paragraph_pairs(before, after) {
        if b.translated_text
            .as_deref()
            .is_none_or(|t| text_equivalent(t, &source_text(a)))
        {
            continue;
        }
        if !is_translatable(a) {
            policy += 1;
        }
        if looks_like_superscript(a) {
            superscript += 1;
        }
        let text = source_text(a);
        if is_numbering(&text) {
            numbering += 1;
        }
        if text.trim().is_empty() {
            blank += 1;
        }
    }
    let weighted = (policy * 10 + superscript * 5 + numbering * 5 + blank * 5) as f64;
    dimension(
        &["OVR-01", "OVR-02", "OVR-03", "OVR-04"],
        weighted,
        denominator,
        values(&[
            ("policy_passthrough_changed", policy),
            ("superscript_candidates", superscript),
            ("numbering_candidates", numbering),
            ("blank_content_changed", blank),
        ]),
    )
}

fn risk(before: &Il, events: &[Value], denominator: f64) -> Dimension {
    let weak = before
        .pages
        .iter()
        .flat_map(|p| &p.paragraphs)
        .filter(|p| is_translatable(p) && p.text.chars.iter().any(|c| c.unicode.is_none()))
        .count();
    let placeholders = count_diagnostic(events, "placeholder_violation");
    let echoes = count_summary_array(events, "suspicious_echoes");
    let weighted = (weak * 5 + placeholders * 10 + echoes * 5) as f64;
    dimension(
        &["RSK-01", "RSK-02", "RSK-03"],
        weighted,
        denominator,
        values(&[
            ("weak_reliability_paragraphs", weak),
            ("placeholder_violations", placeholders),
            ("suspicious_echoes", echoes),
        ]),
    )
}

fn layout(before: &Il, after: &Il, events: &[Value], denominator: f64) -> Dimension {
    let mut offsets = Vec::new();
    let mut ious = Vec::new();
    let mut font_ratios = Vec::new();
    for (a, b) in paragraph_pairs(before, after) {
        if b.translated_text.is_none() {
            continue;
        }
        offsets.push(
            ((a.bounds.left - b.bounds.left).powi(2) + (a.bounds.bottom - b.bounds.bottom).powi(2))
                .sqrt(),
        );
        ious.push(iou(a.bounds, b.bounds));
        let af = median_font(a);
        let bf = median_font(b);
        if af > 0.0 && bf > 0.0 {
            font_ratios.push(bf / af);
        }
    }
    let expansions = count_diagnostic(events, "single_line_bounds_expanded")
        + count_diagnostic(events, "multi_line_bounds_expanded");
    let drifted =
        offsets.iter().filter(|v| **v > 1.0).count() + ious.iter().filter(|v| **v < 0.8).count();
    let weighted = (drifted * 3 + expansions) as f64;
    let mut m = BTreeMap::new();
    m.insert("median_offset_pt".into(), median(&mut offsets).into());
    m.insert("median_iou".into(), median(&mut ious).into());
    m.insert("median_font_scale".into(), median(&mut font_ratios).into());
    m.insert("bounds_expansions".into(), expansions.into());
    dimension(
        &["LAY-01", "LAY-02", "LAY-03", "LAY-04"],
        weighted,
        denominator,
        m,
    )
}

fn lint(after: &Il, events: &[Value], denominator: f64) -> Dimension {
    let texts: Vec<&str> = after
        .pages
        .iter()
        .flat_map(|p| &p.paragraphs)
        .filter_map(|p| p.translated_text.as_deref())
        .collect();
    let forbidden_start = "，。！？；：、）】》”’";
    let forbidden_end = "（【《“‘";
    let kinsoku = texts
        .iter()
        .filter(|t| {
            t.starts_with(|c| forbidden_start.contains(c))
                || t.ends_with(|c| forbidden_end.contains(c))
        })
        .count();
    let isolated = texts
        .iter()
        .filter(|t| t.chars().count() == 1 && t.chars().all(is_punctuation))
        .count();
    let residues = texts
        .iter()
        .map(|t| {
            ["{v", "{l", "<b"]
                .iter()
                .filter(|x| t.contains(**x))
                .count()
        })
        .sum::<usize>();
    let whitespace = texts
        .iter()
        .filter(|t| {
            t.contains("  ")
                || t.starts_with(char::is_whitespace)
                || t.ends_with(char::is_whitespace)
        })
        .count();
    let echoes = count_summary_array(events, "suspicious_echoes");
    let weighted = (kinsoku * 3 + isolated + residues * 10 + whitespace + echoes) as f64;
    dimension(
        &["LNT-01", "LNT-02", "LNT-03", "LNT-04", "LNT-05"],
        weighted,
        denominator,
        values(&[
            ("kinsoku_violations", kinsoku),
            ("isolated_punctuation", isolated),
            ("placeholder_residue", residues),
            ("abnormal_whitespace", whitespace),
            ("english_residual_proxy", echoes),
        ]),
    )
}

fn structure(e: &Evidence, denominator: f64) -> Dimension {
    let binaries = [
        e.qpdf_input_ok,
        e.qpdf_output_ok,
        e.input_is_output_prefix,
        e.page_count_equal,
        e.non_text_object_counts_equal,
        e.non_text_pixel_fidelity.is_some(),
    ];
    let failed = binaries.iter().filter(|v| !**v).count();
    let pixel_error = e
        .non_text_pixel_fidelity
        .map_or(1000.0, |v| (1.0 - v).max(0.0) * 1000.0);
    let weighted = failed as f64 * 100.0 + pixel_error * 10.0;
    let mut m = BTreeMap::new();
    m.insert("binary_checks_failed".into(), failed.into());
    m.insert(
        "masked_non_text_pixel_fidelity".into(),
        e.non_text_pixel_fidelity.into(),
    );
    dimension(
        &["STR-01", "STR-02", "STR-03", "STR-04"],
        weighted,
        denominator,
        m,
    )
}

fn dimension(
    ids: &[&'static str],
    weighted: f64,
    denominator: f64,
    measurements: BTreeMap<String, Value>,
) -> Dimension {
    let rate = weighted * 1000.0 / denominator;
    Dimension {
        formula_ids: ids.to_vec(),
        weighted_errors: round6(weighted),
        errors_per_1000_output_characters: round6(rate),
        score: round6((100.0 - rate).clamp(0.0, 100.0)),
        measurements,
    }
}

fn paragraph_pairs<'a>(a: &'a Il, b: &'a Il) -> Vec<(&'a Paragraph, &'a Paragraph)> {
    let after = b
        .pages
        .iter()
        .flat_map(|page| {
            page.paragraphs
                .iter()
                .map(move |paragraph| ((page.index, paragraph.reading_order), paragraph))
        })
        .collect::<BTreeMap<_, _>>();
    a.pages
        .iter()
        .flat_map(|page| {
            page.paragraphs.iter().filter_map(|paragraph| {
                after
                    .get(&(page.index, paragraph.reading_order))
                    .map(|other| (paragraph, *other))
            })
        })
        .collect()
}
fn source_text(p: &Paragraph) -> String {
    p.text
        .chars
        .iter()
        .filter_map(|c| c.unicode.as_deref())
        .collect()
}
fn find_paragraph(il: &Il, page_index: usize, reading_order: usize) -> Option<&Paragraph> {
    il.pages
        .iter()
        .find(|page| page.index == page_index)?
        .paragraphs
        .iter()
        .find(|paragraph| paragraph.reading_order == reading_order)
}
fn count_han(text: &str) -> usize {
    text.chars().filter(|c| is_han(*c)).count()
}
fn is_han(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x3134f
    )
}
fn text_equivalent(a: &str, b: &str) -> bool {
    a.chars()
        .filter(|c| !c.is_whitespace())
        .eq(b.chars().filter(|c| !c.is_whitespace()))
}
fn is_translatable(p: &Paragraph) -> bool {
    p.text
        .chars
        .iter()
        .filter_map(|c| c.layout.as_ref())
        .any(|l| l.policy == "translate")
}
fn preserved_reason(p: &Paragraph) -> Option<String> {
    p.preserved
        .as_ref()
        .and_then(|v| v.get("reason").or(Some(v)))
        .and_then(Value::as_str)
        .map(str::to_owned)
}
fn looks_like_superscript(p: &Paragraph) -> bool {
    if p.text.chars.len() < 2 {
        return false;
    }
    let mut sizes: Vec<f64> = p.text.chars.iter().map(|c| c.font_size).collect();
    let bases: Vec<f64> = p.text.chars.iter().map(|c| c.baseline_origin.y).collect();
    let median_size = median(&mut sizes);
    let mut sorted_bases = bases.clone();
    let median_base = median(&mut sorted_bases);
    p.text.chars.iter().any(|c| {
        c.font_size <= median_size * 0.8
            && (c.baseline_origin.y - median_base).abs() >= median_size * 0.15
    })
}
fn is_numbering(t: &str) -> bool {
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || ".()[]- ".contains(c))
}
fn median_font(p: &Paragraph) -> f64 {
    let mut v: Vec<f64> = p.text.chars.iter().map(|c| c.font_size).collect();
    median(&mut v)
}
fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n.is_multiple_of(2) {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    } else {
        v[n / 2]
    }
}
fn iou(a: Rect, b: Rect) -> f64 {
    let iw = (a.right.min(b.right) - a.left.max(b.left)).max(0.0);
    let ih = (a.top.min(b.top) - a.bottom.max(b.bottom)).max(0.0);
    let i = iw * ih;
    let u = (a.right - a.left).max(0.0) * (a.top - a.bottom).max(0.0)
        + (b.right - b.left).max(0.0) * (b.top - b.bottom).max(0.0)
        - i;
    ratio(i, u)
}
fn ratio(a: f64, b: f64) -> f64 {
    if b == 0.0 { 1.0 } else { round6(a / b) }
}
fn round6(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation() || "，。！？；：、（）【】《》“”‘’".contains(c)
}
fn values(items: &[(&str, usize)]) -> BTreeMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), (*v).into()))
        .collect()
}

fn count_diagnostic(events: &[Value], id: &str) -> usize {
    events
        .iter()
        .filter(|v| {
            v.get("event").and_then(Value::as_str) == Some("diagnostic")
                && v.get("id").and_then(Value::as_str) == Some(id)
        })
        .count()
}
fn count_summary_array(events: &[Value], key: &str) -> usize {
    events
        .iter()
        .find_map(|v| v.get(key).and_then(Value::as_array).map(Vec::len))
        .unwrap_or(0)
}
fn read_ndjson(path: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(Into::into))
        .collect()
}
fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T> {
    serde_json::from_slice(&fs::read(&path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?
    }
    let mut f = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    f.write_all(bytes)?;
    f.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn qpdf_check(path: &Path) -> bool {
    Command::new("qpdf")
        .arg("--check")
        .arg(path)
        .output()
        .is_ok_and(|o| o.status.success())
}
fn extracted_character_count(path: &Path) -> Result<usize> {
    let output = Command::new("pdftotext").arg(path).arg("-").output()?;
    if !output.status.success() {
        bail!(
            "pdftotext failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .chars()
        .filter(|c| !c.is_whitespace())
        .count())
}
fn qpdf_pages(path: &Path) -> Option<usize> {
    let o = Command::new("qpdf")
        .arg("--show-npages")
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8(o.stdout).ok()?.trim().parse().ok()
}
fn qpdf_non_text_counts(path: &Path) -> Result<BTreeMap<String, usize>> {
    let o = Command::new("qpdf").arg("--json").arg(path).output()?;
    if !o.status.success() {
        bail!("qpdf --json failed for {}", path.display())
    }
    let v: Value = serde_json::from_slice(&o.stdout)?;
    let mut out = BTreeMap::new();
    count_json_names(&v, &mut out);
    Ok(out)
}
fn count_json_names(v: &Value, out: &mut BTreeMap<String, usize>) {
    match v {
        Value::String(s)
            if ["/Form", "/Image", "/Link", "/Annot", "/Outlines"].contains(&s.as_str()) =>
        {
            *out.entry(s.clone()).or_default() += 1
        }
        Value::Array(a) => a.iter().for_each(|v| count_json_names(v, out)),
        Value::Object(m) => m.values().for_each(|v| count_json_names(v, out)),
        _ => {}
    }
}

fn translated_masks(il: &Il) -> BTreeMap<usize, Vec<Rect>> {
    il.pages
        .iter()
        .map(|p| {
            (
                p.index,
                p.paragraphs
                    .iter()
                    .filter(|x| x.translated_text.is_some() && x.preserved.is_none())
                    .map(|x| x.bounds)
                    .collect(),
            )
        })
        .collect()
}
struct Pix {
    w: usize,
    h: usize,
    data: Vec<u8>,
}
fn pixel_fidelity(
    input: &Path,
    output: &Path,
    masks: &BTreeMap<usize, Vec<Rect>>,
    dpi: u32,
) -> Result<f64> {
    let dir = tempfile::tempdir()?;
    let a = dir.path().join("in");
    let b = dir.path().join("out");
    render(input, &a, dpi)?;
    render(output, &b, dpi)?;
    let input_pages = rendered_pages(dir.path(), "in-")?;
    let output_pages = rendered_pages(dir.path(), "out-")?;
    if input_pages.len() != output_pages.len() {
        bail!("rendered page counts differ")
    }
    let mut same = 0u64;
    let mut total = 0u64;
    for (page_index, (ap, bp)) in input_pages.iter().zip(&output_pages).enumerate() {
        let x = parse_ppm(ap)?;
        let y = parse_ppm(bp)?;
        if x.w != y.w || x.h != y.h {
            bail!("render dimensions differ on page {}", page_index + 1)
        }
        let scale = dpi as f64 / 72.0;
        for py in 0..x.h {
            for px in 0..x.w {
                let pdf_x = px as f64 / scale;
                let pdf_y = (x.h - py) as f64 / scale;
                if masks.get(&page_index).is_some_and(|rs| {
                    rs.iter().any(|r| {
                        pdf_x >= r.left - 2.0
                            && pdf_x <= r.right + 2.0
                            && pdf_y >= r.bottom - 2.0
                            && pdf_y <= r.top + 2.0
                    })
                }) {
                    continue;
                }
                let i = (py * x.w + px) * 3;
                total += 1;
                if x.data[i..i + 3] == y.data[i..i + 3] {
                    same += 1
                }
            }
        }
    }
    Ok(ratio(same as f64, total as f64))
}
fn rendered_pages(dir: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".ppm"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}
fn render(pdf: &Path, prefix: &Path, dpi: u32) -> Result<()> {
    let o = Command::new("pdftoppm")
        .args(["-r", &dpi.to_string(), "-aa", "no", "-aaVector", "no"])
        .arg(pdf)
        .arg(prefix)
        .output()?;
    if !o.status.success() {
        bail!("pdftoppm failed: {}", String::from_utf8_lossy(&o.stderr))
    }
    Ok(())
}
fn parse_ppm(path: &Path) -> Result<Pix> {
    let bytes = fs::read(path)?;
    let mut ends = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            ends.push(i);
            if ends.len() == 3 {
                break;
            }
        }
    }
    if ends.len() < 3 || &bytes[..2] != b"P6" {
        bail!("unsupported PPM {}", path.display())
    }
    let dims = std::str::from_utf8(&bytes[ends[0] + 1..ends[1]])?;
    let mut it = dims.split_whitespace();
    let w = it.next().context("PPM width")?.parse()?;
    let h = it.next().context("PPM height")?.parse()?;
    if &bytes[ends[1] + 1..ends[2]] != b"255" {
        bail!("unsupported PPM depth")
    }
    Ok(Pix {
        w,
        h,
        data: bytes[ends[2] + 1..].to_vec(),
    })
}

fn markdown(r: &Report) -> String {
    let mut s = String::from(
        "# Quality scorecard\n\n| Dimension | Score | Weighted errors / 1k chars |\n| --- | ---: | ---: |\n",
    );
    for (k, v) in &r.dimensions {
        s.push_str(&format!(
            "| {k} | {:.3} | {:.3} |\n",
            v.score, v.errors_per_1000_output_characters
        ));
    }
    s.push_str(&format!(
        "| **total** | **{:.3}** | |\n\nOutput characters: {}. Schema: v{}.\n",
        r.total_score, r.output_characters, r.schema_version
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn il(policy: &str, translated_text: Option<&str>, preserved: Option<Value>) -> Il {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "pages": [{
                "index": 0,
                "geometry": {"width": 100.0, "height": 100.0},
                "paragraphs": [{
                    "reading_order": 7,
                    "bounds": {"left": 1.0, "bottom": 2.0, "right": 9.0, "top": 8.0},
                    "text": {"kind": "chars", "chars": [{
                        "unicode": "A",
                        "font_size": 10.0,
                        "baseline_origin": {"x": 1.0, "y": 2.0},
                        "layout": {"label": "text", "policy": policy}
                    }]},
                    "translated_text": translated_text,
                    "preserved": preserved
                }]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn hand_built_artifact_reports_typed_coverage_gap() {
        let before = il("translate", None, None);
        let after = il(
            "translate",
            Some("A"),
            Some(Value::String("unreliable_unicode".into())),
        );
        let result = coverage(&before, &after, &after, 1000.0);
        assert_eq!(result.weighted_errors, 5.0);
        assert_eq!(result.measurements["paragraph_coverage"], 0.0);
        assert_eq!(
            result.measurements["preserved_reasons"]["unreliable_unicode"],
            1
        );
    }

    #[test]
    fn mixed_paragraph_is_eligible_when_any_unit_translates() {
        let value = serde_json::json!({
            "pages": [{"index": 0, "paragraphs": [{
                "reading_order": 0,
                "bounds": {"left": 0.0, "bottom": 0.0, "right": 2.0, "top": 1.0},
                "text": {"chars": [
                    {"unicode":"1", "font_size":8.0, "baseline_origin":{"y":0.0}, "layout":{"label":"number", "policy":"passthrough"}},
                    {"unicode":"A", "font_size":8.0, "baseline_origin":{"y":0.0}, "layout":{"label":"text", "policy":"translate"}}
                ]}
            }]}]
        });
        let document: Il = serde_json::from_value(value).unwrap();
        assert!(is_translatable(&document.pages[0].paragraphs[0]));
    }

    #[test]
    fn whitespace_reconstruction_is_not_overtranslation() {
        assert!(text_equivalent("Google Brain", "GoogleBrain"));
    }
}
