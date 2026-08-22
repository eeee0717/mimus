//! `corpus verify` / `corpus adjudicate` —— §2.8 的五步独立验收。
//!
//! 失败信息一律带上 fixture ID 与合同条款号。语料的失败大多不是「代码有 bug」，
//! 而是「这份 fixture 违反了某条约定」——不指出是哪一条，排查就得从头读一遍
//! 合同。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::adjudicated::{Adjudicated, BlockGeometry, RenderReference};
use crate::determinism;
use crate::geom::{PageFrame, close};
use crate::hash;
use crate::manifest::{Check, GeometrySource, Legality, Manifest, Method};
use crate::oracle::render::PageRaster;
use crate::oracle::{ParsedPage, mupdf, poppler, qpdf, render};
use crate::text;
use crate::toolchain::Toolchain;

/// 单条检查的结论。
pub struct Outcome {
    pub check: &'static str,
    pub clause: &'static str,
    pub passed: bool,
    pub detail: String,
}

impl Outcome {
    fn ok(check: &'static str, clause: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            clause,
            passed: true,
            detail: detail.into(),
        }
    }

    fn fail(check: &'static str, clause: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            clause,
            passed: false,
            detail: detail.into(),
        }
    }
}

/// 一份 fixture 的验收结论 + 后续跨 fixture 断言所需的观测。
pub struct Report {
    pub fixture: String,
    pub outcomes: Vec<Outcome>,
    /// mutool 眼中的绘制顺序文本序列，逐页。
    pub draw_order: Vec<Vec<String>>,
    /// 逐页参考栅格。
    pub raster: Vec<PageRaster>,
    /// 本次裁定出的页面空间块几何；未声明 dual-parser-geometry 时为空。
    pub geometry: Vec<BlockGeometry>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(|o| o.passed)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 复核已记录的裁定结果。
    Verify,
    /// 重新裁定并写入 `adjudicated.toml`。
    Adjudicate,
}

/// 枚举 `corpus/fixtures/` 下的全部 fixture。
pub fn discover(repo_root: &Path) -> Result<Vec<Manifest>> {
    let root = repo_root.join("corpus/fixtures");
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .with_context(|| format!("读取 {} 失败", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("manifest.toml").is_file())
        .collect();
    dirs.sort();

    dirs.iter().map(|d| Manifest::load(d)).collect()
}

/// 跑一批 fixture 的验收；返回 `true` 表示全部通过。
pub fn run(
    manifests: &[Manifest],
    toolchain: &Toolchain,
    repo_root: &Path,
    work_dir: &Path,
    mode: Mode,
) -> Result<bool> {
    if manifests.is_empty() {
        println!("corpus/fixtures/ 下没有 fixture。");
        return Ok(true);
    }

    let mut reports = Vec::new();
    for manifest in manifests {
        let mut report = verify_one(manifest, toolchain, repo_root, work_dir, mode)?;
        report
            .outcomes
            .insert(0, check_lineage(manifest, manifests));
        print_report(manifest, &report);
        reports.push(report);
    }

    let group = check_groups(manifests, &reports);
    if !group.is_empty() {
        println!("\n跨 fixture 断言：");
        for outcome in &group {
            print_outcome(&outcome.1);
            if !outcome.1.passed {
                println!("         fixture: {}", outcome.0);
            }
        }
    }

    let all_ok = reports.iter().all(Report::passed) && group.iter().all(|(_, o)| o.passed);
    println!(
        "\n{} 份 fixture，{}",
        manifests.len(),
        if all_ok {
            "全部通过 §2.8 独立验收".to_string()
        } else {
            "存在未通过项——未通过的 fixture 不得入库".to_string()
        }
    );
    Ok(all_ok)
}

fn print_report(manifest: &Manifest, report: &Report) {
    println!(
        "\n{}  [{:?}] {}",
        report.fixture, manifest.identity.priority, manifest.identity.name
    );
    println!("       变量：{}", manifest.identity.variable);
    // 期望行为是给 mimus 的断言（§2.9），本工具管不到它们；但把它们打出来是
    // 有意义的——manifest 的这部分内容否则永远没有被看见的时刻。
    for b in &manifest.expected.behaviour {
        println!(
            "       期望行为 {}：{}（观察手段：{}）",
            b.id, b.assertion, b.observable_via
        );
    }
    for a in &manifest.adjudication {
        println!("       裁定 {}：{} → {}", a.date, a.issue, a.resolution);
    }
    for outcome in &report.outcomes {
        print_outcome(outcome);
    }
}

/// 畸形 fixture 的合法父本必须真的在语料里（§2.5）——「有个父本」这句话
/// 只有在父本可被指认时才有意义。
fn check_lineage(manifest: &Manifest, all: &[Manifest]) -> Outcome {
    const CHECK: &str = "lineage";
    const CLAUSE: &str = "§2.5";

    let Some(lineage) = &manifest.lineage else {
        return Outcome::ok(CHECK, CLAUSE, "合法 fixture，无谱系");
    };
    if all.iter().any(|m| m.id() == lineage.parent) {
        Outcome::ok(
            CHECK,
            CLAUSE,
            format!(
                "父本 `{}`，{} 处变异",
                lineage.parent,
                lineage.mutations.len()
            ),
        )
    } else {
        Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "声明的合法父本 `{}` 不在本次验收的 fixture 集合里",
                lineage.parent
            ),
        )
    }
}

fn print_outcome(outcome: &Outcome) {
    println!(
        "  [{}] {:<20} {:<7} {}",
        if outcome.passed { "ok  " } else { "FAIL" },
        outcome.check,
        outcome.clause,
        outcome.detail
    );
}

fn verify_one(
    manifest: &Manifest,
    toolchain: &Toolchain,
    repo_root: &Path,
    work_dir: &Path,
    mode: Mode,
) -> Result<Report> {
    let mut outcomes = Vec::new();
    let pdf = manifest.pdf_path();
    let fixture_work = work_dir.join("fixtures").join(manifest.id());

    if !pdf.is_file() {
        bail!("[{}] PDF 不存在：{}", manifest.id(), pdf.display());
    }

    let frames: Vec<PageFrame> = manifest
        .page
        .iter()
        .map(|p| PageFrame::new(p.effective_box(), p.rotate))
        .collect::<Result<_>>()
        .with_context(|| format!("[{}] 页面框无效", manifest.id()))?;

    outcomes.push(check_pins(manifest)?);

    if manifest.requires(Check::Determinism) {
        outcomes.push(check_determinism(
            manifest,
            toolchain,
            repo_root,
            &fixture_work,
        )?);
    }
    if manifest.requires(Check::Legality) {
        outcomes.push(check_legality(manifest, &pdf)?);
    }
    if manifest.requires(Check::PageGeometry) {
        outcomes.extend(check_page_geometry(manifest, &pdf, &frames)?);
    }

    // 结构、几何裁定与渲染共用同一批解析结果，一次算好。
    let mutool_pages = mupdf::blocks(&pdf, &frames)?;
    let poppler_pages = poppler::blocks(&pdf, &frames)?;

    if manifest.requires(Check::Structure) {
        outcomes.extend(check_structure(manifest, &mutool_pages, &poppler_pages));
    }
    if manifest.requires(Check::Glyphs) {
        outcomes.extend(check_glyphs(manifest, &mutool_pages, &poppler_pages));
    }
    if manifest.requires(Check::ReadingOrder) {
        outcomes.push(check_reading_order(manifest, &poppler_pages));
    }

    let raster = if manifest.requires(Check::Render) {
        render::rasterize(&pdf, &fixture_work.join("raster"), manifest.page.len())?
    } else {
        Vec::new()
    };

    if manifest.requires(Check::Render) && manifest.requires(Check::PageGeometry) {
        outcomes.push(check_raster_orientation(&frames, &raster));
    }

    // 几何裁定与参考栅格共用一份 adjudicated.toml：两者都是**测出来的**结果，
    // 与手写的 manifest 分开存放（§2.1）。因此落盘与复核也只有一处。
    let geometry = if manifest.requires(Check::DualParserGeometry) {
        let (outcome, blocks) = check_dual_parser_geometry(manifest, &mutool_pages, &poppler_pages);
        outcomes.push(outcome);
        blocks
    } else {
        Some(Vec::new())
    };
    if manifest.requires(Check::DualParserGeometry) || manifest.requires(Check::Render) {
        outcomes.push(record_adjudicated(
            manifest,
            geometry.as_deref(),
            &raster,
            mode,
        )?);
    }
    let geometry = geometry.unwrap_or_default();

    let draw_order = mutool_pages
        .iter()
        .map(|p| p.blocks.iter().map(|b| b.text.clone()).collect())
        .collect();

    Ok(Report {
        fixture: manifest.id().to_string(),
        outcomes,
        draw_order,
        raster,
        geometry,
    })
}

// ---------------------------------------------------------------- 钉死项

/// 校验 manifest 里那些「声明了就必须为真」的钉死项：vendored 字体的 SHA-256、
/// 畸形 fixture 的变异偏移是否落在文件内。
///
/// 这些字段本身不参与后续计算，但它们是 §2.6 / §2.7 的必备信息；不校验就等于
/// 允许它们慢慢变成过期的注释。
fn check_pins(manifest: &Manifest) -> Result<Outcome> {
    const CHECK: &str = "pins";
    const CLAUSE: &str = "§2.6/§2.7";

    let mut problems = Vec::new();
    let mut checked = 0usize;

    for font in &manifest.source.fonts {
        let path = manifest.dir.join(&font.file);
        if !path.is_file() {
            problems.push(format!("钉死的字体文件不存在：{}", path.display()));
            continue;
        }
        let actual = hash::of_file(&path)?;
        if actual != font.sha256 {
            problems.push(format!(
                "字体 {} 的 SHA-256 不符：声明 {}，实际 {actual}",
                font.file, font.sha256
            ));
        }
        checked += 1;
    }

    if let Some(lineage) = &manifest.lineage {
        let size = std::fs::metadata(manifest.pdf_path())?.len();
        for mutation in &lineage.mutations {
            if mutation.byte_offset >= size {
                problems.push(format!(
                    "变异偏移 {} 超出 PDF 长度 {size}（描述：{}）",
                    mutation.byte_offset, mutation.description
                ));
            }
            checked += 1;
        }
    }

    Ok(if problems.is_empty() {
        Outcome::ok(CHECK, CLAUSE, format!("{checked} 项钉死值核对通过"))
    } else {
        Outcome::fail(CHECK, CLAUSE, problems.join("；"))
    })
}

// ---------------------------------------------------------------- §2.8 步骤 1

fn check_determinism(
    manifest: &Manifest,
    toolchain: &Toolchain,
    repo_root: &Path,
    work_dir: &Path,
) -> Result<Outcome> {
    const CHECK: &str = "determinism";
    const CLAUSE: &str = "§2.6";

    let committed = hash::of_file(&manifest.pdf_path())?;
    if committed != manifest.source.pdf_sha256 {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "入库 PDF 的 SHA-256 与 manifest 声明不符：文件 {committed}，manifest {}",
                manifest.source.pdf_sha256
            ),
        ));
    }

    if manifest.source.method != Method::RealisticTypesetting {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "method = {:?} 的重复生成尚未实现（本里程碑只交付现实排版路径）",
                manifest.source.method
            ),
        ));
    }

    let engine_id = manifest
        .source
        .engine
        .as_deref()
        .context("现实排版 fixture 缺少 engine")?;
    let Some(engine) = toolchain.engine.iter().find(|e| e.id == engine_id) else {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!("engine `{engine_id}` 不在 corpus/toolchain.toml 里"),
        ));
    };
    if !engine.corpus_v1_usable {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!("engine `{engine_id}` 已被判定不可用于 Corpus v1"),
        ));
    }

    let source = manifest.source_path(repo_root)?;
    let mut hashes = Vec::new();
    for slot in ["rebuild-a", "rebuild-b"] {
        let outdir = work_dir.join(slot);
        if outdir.exists() {
            std::fs::remove_dir_all(&outdir)?;
        }
        let built = determinism::build_source(engine, repo_root, &source, &outdir)?;
        hashes.push(hash::of_file(&built)?);
        if slot == "rebuild-a" {
            std::thread::sleep(determinism::DEFAULT_GAP);
        }
    }

    if hashes[0] != hashes[1] {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!("重复生成不一致：{} vs {}", hashes[0], hashes[1]),
        ));
    }
    if hashes[0] != committed {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!("重新生成得到 {}，与入库 PDF {committed} 不符", hashes[0]),
        ));
    }

    Ok(Outcome::ok(
        CHECK,
        CLAUSE,
        format!(
            "重复生成 2 次一致，且与入库 PDF 相同（{}）",
            &committed[..16]
        ),
    ))
}

// ---------------------------------------------------------------- §2.8 步骤 2

fn check_legality(manifest: &Manifest, pdf: &Path) -> Result<Outcome> {
    const CHECK: &str = "legality";
    const CLAUSE: &str = "§2.8";

    let result = qpdf::check(pdf)?;
    match manifest.identity.legality {
        Legality::Legal => Ok(if result.passed {
            Outcome::ok(CHECK, CLAUSE, "qpdf --check 无错误无警告")
        } else {
            Outcome::fail(
                CHECK,
                CLAUSE,
                format!(
                    "合法 fixture 未通过 qpdf --check：\n{}",
                    indent(&result.report)
                ),
            )
        }),
        Legality::Malformed => {
            let declared = manifest
                .expected
                .declared_failure
                .as_deref()
                .context("畸形 fixture 缺少 declared_failure")?;
            if result.passed {
                return Ok(Outcome::fail(
                    CHECK,
                    CLAUSE,
                    "畸形 fixture 却通过了 qpdf --check——变异没打中目标",
                ));
            }
            Ok(if result.report.contains(declared) {
                Outcome::ok(CHECK, CLAUSE, format!("以声明的方式失败：{declared:?}"))
            } else {
                Outcome::fail(
                    CHECK,
                    CLAUSE,
                    format!(
                        "失败方式与声明不符。声明 {declared:?}，实际：\n{}",
                        indent(&result.report)
                    ),
                )
            })
        }
    }
}

// ---------------------------------------------------------------- §2.2 / §2.3

fn check_page_geometry(
    manifest: &Manifest,
    pdf: &Path,
    frames: &[PageFrame],
) -> Result<Vec<Outcome>> {
    const CHECK: &str = "page-geometry";
    const CLAUSE: &str = "§2.2/§2.3";

    let observed = mupdf::pages(pdf)?;
    if observed.len() != manifest.page.len() {
        return Ok(vec![Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "页数不符：manifest {} 页，mutool 报 {} 页",
                manifest.page.len(),
                observed.len()
            ),
        )]);
    }

    let tol = manifest.expected.tolerance_pt;
    let mut problems = Vec::new();
    for (page, seen) in manifest.page.iter().zip(&observed) {
        if !arrays_close(&page.media_box, &seen.media_box, tol) {
            problems.push(format!(
                "第 {} 页 MediaBox：manifest {:?}，mutool {:?}",
                page.index, page.media_box, seen.media_box
            ));
        }
        match (page.crop_box, seen.crop_box) {
            (Some(a), Some(b)) if !arrays_close(&a, &b, tol) => problems.push(format!(
                "第 {} 页 CropBox：manifest {a:?}，mutool {b:?}",
                page.index
            )),
            (a, b) if a.is_some() != b.is_some() => problems.push(format!(
                "第 {} 页 CropBox 有无不一致：manifest {a:?}，mutool {b:?}",
                page.index
            )),
            _ => {}
        }
        if page.rotate.rem_euclid(360) != seen.rotate.rem_euclid(360) {
            problems.push(format!(
                "第 {} 页 /Rotate：manifest {}，mutool {}",
                page.index, page.rotate, seen.rotate
            ));
        }
    }

    let mut outcomes = vec![if problems.is_empty() {
        Outcome::ok(
            CHECK,
            CLAUSE,
            format!(
                "{} 页的 MediaBox / CropBox / Rotate 与 manifest 一致",
                observed.len()
            ),
        )
    } else {
        Outcome::fail(CHECK, CLAUSE, problems.join("；"))
    }];

    // 第二个解析器：poppler 独立报出有效框的**尺寸**。它不覆盖 /Rotate——
    // 见 `PageFrame::box_size` 的实测记录：`-bbox-layout` 的 <page width height>
    // 不应用 /Rotate，而同一份输出里的坐标应用了。/Rotate 的正确性改由
    // group/geom-equal 断言（五个取值换算回页面空间后必须逐块相同），那是比
    // 一对宽高更强的判据。
    let poppler_pages = poppler::blocks(pdf, frames)?;
    let mut size_problems = Vec::new();
    for (frame, page) in frames.iter().zip(&poppler_pages) {
        let (w, h) = frame.box_size();
        if !close(w, page.viewer_size.0, tol) || !close(h, page.viewer_size.1, tol) {
            size_problems.push(format!(
                "第 {} 页有效框尺寸：manifest 推得 {w}×{h}，poppler 报 {}×{}",
                page.index, page.viewer_size.0, page.viewer_size.1
            ));
        }
    }
    outcomes.push(if size_problems.is_empty() {
        Outcome::ok(
            CHECK,
            "§2.3",
            "poppler 独立报出的有效框尺寸与 manifest 一致",
        )
    } else {
        Outcome::fail(CHECK, "§2.3", size_problems.join("；"))
    });

    Ok(outcomes)
}

/// 独立渲染器出图的像素朝向是否与 manifest 的 `/Rotate` 一致（§2.3）。
///
/// 这是 `/Rotate` 唯一一条**单份 fixture 内部**可判的证据。解析器侧判不了：
/// poppler 的 `<page width height>` 不应用 /Rotate（见 `PageFrame::box_size`）。
/// 渲染器侧则毫无歧义——`/Rotate 90` 的 300×200 页面出的就是竖图。
fn check_raster_orientation(frames: &[PageFrame], raster: &[PageRaster]) -> Outcome {
    const CHECK: &str = "raster-orientation";
    const CLAUSE: &str = "§2.3";

    // pdftoppm 按 dpi 取整，容许 1 px 的取整差；90° 交换会差出几百 px，
    // 这点容差挡不住它。
    const SLACK: i64 = 1;

    let mut problems = Vec::new();
    for (i, (frame, page)) in frames.iter().zip(raster).enumerate() {
        let (w, h) = frame.viewer_size();
        let px = |pt: f64| (pt * f64::from(render::DPI) / 72.0).round() as i64;
        let (ew, eh) = (px(w), px(h));
        let (aw, ah) = (i64::from(page.pixels.0), i64::from(page.pixels.1));
        if (ew - aw).abs() > SLACK || (eh - ah).abs() > SLACK {
            problems.push(format!(
                "第 {i} 页 {} dpi 出图：manifest 推得 {ew}×{eh} px，pdftoppm 出的是 {aw}×{ah} px",
                render::DPI
            ));
        }
    }

    if problems.is_empty() {
        Outcome::ok(
            CHECK,
            CLAUSE,
            format!(
                "{} 页出图的像素朝向与 manifest 的 /Rotate 一致",
                raster.len()
            ),
        )
    } else {
        Outcome::fail(CHECK, CLAUSE, problems.join("；"))
    }
}

// ---------------------------------------------------------------- §2.1 结构化期望

fn check_structure(
    manifest: &Manifest,
    mutool_pages: &[ParsedPage],
    poppler_pages: &[ParsedPage],
) -> Vec<Outcome> {
    const CLAUSE: &str = "§2.1";

    let expected_draw = ordered_texts(manifest, |b| b.draw_order);

    // poppler 只被要求看到**同一组**块，不被要求同意它们的先后——那是
    // reading-order 检查的事，且并非每份 fixture 都适合断言它。
    let mut expected_set = expected_draw.clone();
    expected_set.sort();
    let mut poppler_set = flatten(poppler_pages);
    poppler_set.sort();

    vec![
        compare_sequence(
            "structure/draw",
            CLAUSE,
            &expected_draw,
            &flatten(mutool_pages),
            "mutool（content stream 绘制顺序）",
        ),
        compare_sequence(
            "structure/set",
            CLAUSE,
            &expected_set,
            &poppler_set,
            "poppler（同一组块，忽略先后）",
        ),
    ]
}

/// 两个解析器各自看到的字符多重集与手写文本相同（不涉及次序）。
fn check_glyphs(
    manifest: &Manifest,
    mutool_pages: &[ParsedPage],
    poppler_pages: &[ParsedPage],
) -> Vec<Outcome> {
    let expected = glyph_counts(
        &manifest
            .expected
            .block
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>(),
    );
    [
        ("glyphs/mutool", mutool_pages),
        ("glyphs/poppler", poppler_pages),
    ]
    .into_iter()
    .map(|(check, pages)| {
        let actual = glyph_counts(&flatten(pages));
        let mut problems = Vec::new();
        for (ch, n) in &expected {
            let got = actual.get(ch).copied().unwrap_or(0);
            if got != *n {
                problems.push(format!("{ch:?}：手写 {n} 个，解析器给出 {got} 个"));
            }
        }
        for (ch, n) in &actual {
            if !expected.contains_key(ch) {
                problems.push(format!("{ch:?}：手写里没有，解析器给出 {n} 个"));
            }
        }
        if problems.is_empty() {
            Outcome::ok(
                check,
                "§2.1",
                format!(
                    "{} 种字符、共 {} 个，与手写文本逐字符相同",
                    expected.len(),
                    expected.values().sum::<usize>()
                ),
            )
        } else {
            Outcome::fail(check, "§2.1", problems.join("；"))
        }
    })
    .collect()
}

fn glyph_counts(texts: &[String]) -> BTreeMap<char, usize> {
    let mut counts = BTreeMap::new();
    for ch in texts
        .iter()
        .flat_map(|t| text::compare_key(t).chars().collect::<Vec<_>>())
    {
        *counts.entry(ch).or_insert(0) += 1;
    }
    counts
}

/// poppler 的版面分析顺序与手写 `reading_order` 一致。
fn check_reading_order(manifest: &Manifest, poppler_pages: &[ParsedPage]) -> Outcome {
    compare_sequence(
        "reading-order",
        "§2.1",
        &ordered_texts(manifest, |b| b.reading_order),
        &flatten(poppler_pages),
        "poppler（版面分析后的阅读顺序）",
    )
}

fn ordered_texts(
    manifest: &Manifest,
    key: impl Fn(&crate::manifest::Block) -> usize,
) -> Vec<String> {
    let mut blocks: Vec<&crate::manifest::Block> = manifest.expected.block.iter().collect();
    blocks.sort_by_key(|b| key(b));
    blocks.iter().map(|b| text::compare_key(&b.text)).collect()
}

fn flatten(pages: &[ParsedPage]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|p| p.blocks.iter().map(|b| text::compare_key(&b.text)))
        .collect()
}

fn compare_sequence(
    check: &'static str,
    clause: &'static str,
    expected: &[String],
    observed: &[String],
    source: &str,
) -> Outcome {
    if expected.len() != observed.len() {
        return Outcome::fail(
            check,
            clause,
            format!(
                "块数不符：手写期望 {} 块，{source} 给出 {} 块。\n{}",
                expected.len(),
                observed.len(),
                indent(&observed.join("\n"))
            ),
        );
    }
    for (i, (e, o)) in expected.iter().zip(observed).enumerate() {
        if e != o {
            return Outcome::fail(
                check,
                clause,
                format!(
                    "第 {} 块文本不符（{source}）：\n    期望 {e:?}\n    实际 {o:?}",
                    i + 1
                ),
            );
        }
    }
    Outcome::ok(
        check,
        clause,
        format!("{} 块与手写期望逐项相同（{source}）", expected.len()),
    )
}

// ---------------------------------------------------------------- §2.1 双解析器裁定

fn check_dual_parser_geometry(
    manifest: &Manifest,
    mutool_pages: &[ParsedPage],
    poppler_pages: &[ParsedPage],
) -> (Outcome, Option<Vec<BlockGeometry>>) {
    const CHECK: &str = "dual-parser-geom";
    const CLAUSE: &str = "§2.1";

    if manifest.expected.geometry_source != GeometrySource::DualParserAdjudicated {
        return (
            Outcome::fail(
                CHECK,
                CLAUSE,
                "本 fixture 声明的是手写几何，不应走双解析器裁定",
            ),
            None,
        );
    }

    let mutool = flatten_blocks(mutool_pages);
    let poppler = flatten_blocks(poppler_pages);
    let tol = manifest.expected.tolerance_pt;
    let ink = manifest.expected.ink_margin();

    let mut blocks = Vec::new();
    let mut problems = Vec::new();

    for block in &manifest.expected.block {
        // 用 manifest 手写的文本去两边各找一块，而不是按次序取第 n 块。
        // 次序本身是另外两条检查（structure/draw 与 reading-order）的断言对象；
        // 拿它来配对等于把「顺序对不对」和「几何对不对」搅在一起——实测
        // poppler 的块顺序在 /Rotate 270 下会翻转，那时几何其实完全正确。
        let key = text::compare_key(&block.text);
        let (m, p) = match (pick(&mutool, &key), pick(&poppler, &key)) {
            (Ok(m), Ok(p)) => (m, p),
            (m, p) => {
                for (who, r) in [("mutool", m), ("poppler", p)] {
                    if let Err(why) = r {
                        problems.push(format!("块 `{}` 在 {who} 的输出里{why}", block.key));
                    }
                }
                continue;
            }
        };

        // 两个解析器共同报告的量只有 x 跨度——文本已由上面的配对保证一致。
        if !close(m.rect.x0, p.rect.x0, tol) || !close(m.rect.x1, p.rect.x1, tol) {
            problems.push(format!(
                "块 `{}` 的 x 跨度不一致（容差 {tol} pt）：mutool [{:.4}, {:.4}]，poppler [{:.4}, {:.4}]",
                block.key, m.rect.x0, m.rect.x1, p.rect.x0, p.rect.x1
            ));
            continue;
        }
        // y 方向两者报的是不同的量（墨迹盒 vs 度量盒），要求相等是错的；
        // 可判定的关系是包含：墨迹必须落在度量盒内（余量见 ink_margin_pt）。
        if !m.rect.contained_in(p.rect, ink) {
            problems.push(format!(
                "块 `{}` 的墨迹盒越出度量盒超过 {ink} pt：mutool {:?}，poppler {:?}",
                block.key,
                m.rect.to_array(),
                p.rect.to_array()
            ));
            continue;
        }
        let Some(baseline) = m.baseline_origin else {
            problems.push(format!("块 `{}` 没有 baseline origin", block.key));
            continue;
        };
        if baseline.y < p.rect.y0 - ink || baseline.y > p.rect.y1 + ink {
            problems.push(format!(
                "块 `{}` 的 baseline y={:.4} 落在度量盒 [{:.4}, {:.4}] 之外",
                block.key, baseline.y, p.rect.y0, p.rect.y1
            ));
            continue;
        }

        blocks.push(BlockGeometry::new(
            &block.key, block.page, p.rect, m.rect, baseline,
        ));
    }

    if !problems.is_empty() {
        // §2.1：「两者不一致即阻止该 fixture 入库」。返回 None，adjudicated.toml
        // 因此不会被写出——不一致的几何一旦落盘就会被当成基线。
        return (
            Outcome::fail(
                CHECK,
                CLAUSE,
                format!(
                    "poppler 与 mutool 不一致，按 §2.1 阻止入库：\n{}",
                    indent(&problems.join("\n"))
                ),
            ),
            None,
        );
    }

    (
        Outcome::ok(
            CHECK,
            CLAUSE,
            format!("{} 块由 poppler 与 mutool 一致裁定", blocks.len()),
        ),
        Some(blocks),
    )
}

/// 把测出来的几何与参考栅格落盘（`adjudicate`）或与已落盘的比对（`verify`）。
fn record_adjudicated(
    manifest: &Manifest,
    blocks: Option<&[BlockGeometry]>,
    raster: &[PageRaster],
    mode: Mode,
) -> Result<Outcome> {
    const CHECK: &str = "adjudicated";
    const CLAUSE: &str = "§2.8";

    let Some(blocks) = blocks else {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            "几何裁定未通过，adjudicated.toml 不予写出",
        ));
    };

    let fresh = Adjudicated {
        schema_version: crate::adjudicated::SUPPORTED_SCHEMA_VERSION,
        fixture: manifest.id().to_string(),
        tolerance_pt: manifest.expected.tolerance_pt,
        block: blocks.to_vec(),
        render: raster.iter().map(RenderReference::from).collect(),
    };

    let path = manifest.adjudicated_path();
    Ok(match mode {
        Mode::Adjudicate => {
            std::fs::write(&path, fresh.to_toml())
                .with_context(|| format!("写入 {} 失败", path.display()))?;
            Outcome::ok(
                CHECK,
                CLAUSE,
                format!(
                    "{} 块几何 + {} 页参考栅格已写入 adjudicated.toml",
                    fresh.block.len(),
                    fresh.render.len()
                ),
            )
        }
        Mode::Verify => {
            let recorded = Adjudicated::load(&path)?;
            let diffs = recorded.differences(&fresh);
            if diffs.is_empty() {
                Outcome::ok(
                    CHECK,
                    CLAUSE,
                    format!(
                        "{} 块几何 + {} 页参考栅格与已记录的一致",
                        fresh.block.len(),
                        fresh.render.len()
                    ),
                )
            } else {
                Outcome::fail(
                    CHECK,
                    CLAUSE,
                    format!("与 adjudicated.toml 不符：\n{}", indent(&diffs.join("\n"))),
                )
            }
        }
    })
}

fn flatten_blocks(pages: &[ParsedPage]) -> Vec<crate::oracle::ParsedBlock> {
    pages.iter().flat_map(|p| p.blocks.clone()).collect()
}

/// 按归一化文本在某个解析器的块列表里挑出**唯一**一块。
///
/// 找不到和找到多个都是失败：前者说明 manifest 与生成结果不符，后者说明这份
/// fixture 里有两块文本完全相同，几何裁定无从分辨该配哪一块——两种情况都不该
/// 静默挑第一个。
fn pick<'a>(
    blocks: &'a [crate::oracle::ParsedBlock],
    key: &str,
) -> Result<&'a crate::oracle::ParsedBlock, String> {
    let mut hits = blocks.iter().filter(|b| text::compare_key(&b.text) == key);
    let first = hits.next().ok_or_else(|| {
        format!(
            "找不到匹配的块。该解析器给出的是：\n{}",
            indent(
                &blocks
                    .iter()
                    .map(|b| b.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        )
    })?;
    if hits.next().is_some() {
        return Err("有多块文本完全相同，无法配对".to_string());
    }
    Ok(first)
}

// ---------------------------------------------------------------- 跨 fixture 断言

fn check_groups(manifests: &[Manifest], reports: &[Report]) -> Vec<(String, Outcome)> {
    const CLAUSE: &str = "§2.7";

    let by_id: BTreeMap<&str, &Report> = reports.iter().map(|r| (r.fixture.as_str(), r)).collect();
    let mut outcomes = Vec::new();

    for manifest in manifests {
        let Some(group) = &manifest.group else {
            continue;
        };
        let Some(here) = by_id.get(manifest.id()) else {
            continue;
        };

        for peer_id in &group.raster_identical_with {
            let check = "group/raster-equal";
            let Some(peer) = by_id.get(peer_id.as_str()) else {
                outcomes.push((
                    manifest.id().to_string(),
                    Outcome::fail(
                        check,
                        CLAUSE,
                        format!("组内 fixture `{peer_id}` 未参与本次验收"),
                    ),
                ));
                continue;
            };
            outcomes.push((
                manifest.id().to_string(),
                if here.raster == peer.raster {
                    Outcome::ok(
                        check,
                        CLAUSE,
                        format!(
                            "与 `{peer_id}` 逐像素一致（{} 组：poppler + mutool）",
                            group.name
                        ),
                    )
                } else {
                    Outcome::fail(
                        check,
                        CLAUSE,
                        format!(
                            "与 `{peer_id}` 渲染不一致：\n    本份 {:?}\n    对照 {:?}",
                            here.raster, peer.raster
                        ),
                    )
                },
            ));
        }

        for peer_id in &group.page_geometry_identical_with {
            let check = "group/geom-equal";
            let Some(peer) = by_id.get(peer_id.as_str()) else {
                outcomes.push((
                    manifest.id().to_string(),
                    Outcome::fail(
                        check,
                        CLAUSE,
                        format!("组内 fixture `{peer_id}` 未参与本次验收"),
                    ),
                ));
                continue;
            };
            let diffs = geometry_differences(
                &here.geometry,
                &peer.geometry,
                manifest.expected.tolerance_pt,
            );
            outcomes.push((
                manifest.id().to_string(),
                if diffs.is_empty() {
                    Outcome::ok(
                        check,
                        CLAUSE,
                        format!(
                            "{} 块的页面空间几何与 `{peer_id}` 逐块相同（{} 组）",
                            here.geometry.len(),
                            group.name
                        ),
                    )
                } else {
                    Outcome::fail(
                        check,
                        CLAUSE,
                        format!(
                            "与 `{peer_id}` 的页面空间几何不同：\n{}",
                            indent(&diffs.join("\n"))
                        ),
                    )
                },
            ));
        }

        for peer_id in &group.draw_order_differs_from {
            let check = "group/draw-differs";
            let Some(peer) = by_id.get(peer_id.as_str()) else {
                outcomes.push((
                    manifest.id().to_string(),
                    Outcome::fail(
                        check,
                        CLAUSE,
                        format!("组内 fixture `{peer_id}` 未参与本次验收"),
                    ),
                ));
                continue;
            };
            outcomes.push((
                manifest.id().to_string(),
                if here.draw_order == peer.draw_order {
                    Outcome::fail(
                        check,
                        CLAUSE,
                        format!(
                            "与 `{peer_id}` 的 content stream 绘制顺序**相同**——\
                             这份顺序变体没有真的换掉绘制次序，实验 1 会因此变成空跑"
                        ),
                    )
                } else {
                    Outcome::ok(check, CLAUSE, format!("绘制顺序确实与 `{peer_id}` 不同"))
                },
            ));
        }
    }

    outcomes
}

// ---------------------------------------------------------------- 杂项

/// 两份裁定结果的页面空间几何是否逐块相同。
///
/// 用的是 fixture 自己声明的裁定容差而不是 `adjudicated.toml` 的复现容差
/// （1e-6）：跨 fixture 比较要穿过一次 `/Rotate` 换算与两个解析器各自的取整，
/// 拿复现容差去卡会把「同一份内容」判成不同。
fn geometry_differences(a: &[BlockGeometry], b: &[BlockGeometry], tol: f64) -> Vec<String> {
    if a.len() != b.len() {
        return vec![format!("块数：本份 {}，对照 {}", a.len(), b.len())];
    }
    let mut out = Vec::new();
    for (x, y) in a.iter().zip(b) {
        if x.key != y.key || x.page != y.page {
            out.push(format!("块身份不同：`{}` vs `{}`", x.key, y.key));
            continue;
        }
        for (field, u, v) in [
            ("metric_box", &x.metric_box[..], &y.metric_box[..]),
            ("visual_bbox", &x.visual_bbox[..], &y.visual_bbox[..]),
            (
                "baseline_origin",
                &x.baseline_origin[..],
                &y.baseline_origin[..],
            ),
        ] {
            if u.iter().zip(v).any(|(p, q)| !close(*p, *q, tol)) {
                out.push(format!("块 `{}` 的 {field}：{u:?} vs {v:?}", x.key));
            }
        }
    }
    out
}

fn arrays_close(a: &[f64; 4], b: &[f64; 4], tol: f64) -> bool {
    a.iter().zip(b).all(|(x, y)| close(*x, *y, tol))
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("       {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------- 生成

/// 用引擎配方把 fixture 的 PDF 生成到它自己的目录里，返回 SHA-256。
///
/// `pdf_sha256` 是**测出来的**值而不是期望值，所以允许 `--write-hash` 机械回填
/// ——24 份 fixture 手抄 24 个哈希只会引入抄错。手写的部分是 manifest 的期望，
/// 那部分工具一个字都不碰。
pub fn build(
    manifests: &[Manifest],
    toolchain: &Toolchain,
    repo_root: &Path,
    write_hash: bool,
) -> Result<bool> {
    for manifest in manifests {
        let engine_id = manifest
            .source
            .engine
            .as_deref()
            .with_context(|| format!("[{}] 没有 engine，无法生成", manifest.id()))?;
        let engine = toolchain
            .engine
            .iter()
            .find(|e| e.id == engine_id)
            .with_context(|| format!("[{}] engine `{engine_id}` 不存在", manifest.id()))?;

        let source = manifest.source_path(repo_root)?;
        let built = determinism::build_source(engine, repo_root, &source, &manifest.dir)?;
        let target = manifest.pdf_path();
        if built != target {
            std::fs::rename(&built, &target).with_context(|| {
                format!("把 {} 挪到 {} 失败", built.display(), target.display())
            })?;
        }
        let sha = hash::of_file(&target)?;

        if write_hash {
            rewrite_pdf_hash(manifest, &sha)?;
        }
        println!(
            "  {:<40} {sha}{}",
            manifest.id(),
            if write_hash { "  (已回填)" } else { "" }
        );
    }
    Ok(true)
}

fn rewrite_pdf_hash(manifest: &Manifest, sha: &str) -> Result<()> {
    let path = manifest.dir.join("manifest.toml");
    let text = std::fs::read_to_string(&path)?;
    let mut replaced = 0;
    let out: Vec<String> = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("pdf_sha256") {
                replaced += 1;
                format!("pdf_sha256 = \"{sha}\"")
            } else {
                line.to_string()
            }
        })
        .collect();
    if replaced != 1 {
        bail!(
            "{} 里有 {replaced} 行 pdf_sha256，期望恰好 1 行",
            path.display()
        );
    }
    std::fs::write(&path, format!("{}\n", out.join("\n")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_sequence_passes() {
        let expected = vec!["a".to_string(), "b".to_string()];
        let observed = expected.clone();
        assert!(compare_sequence("t", "§x", &expected, &observed, "src").passed);
    }

    #[test]
    fn a_permuted_sequence_fails_and_names_the_position() {
        let expected = vec!["a".to_string(), "b".to_string()];
        let observed = vec!["b".to_string(), "a".to_string()];
        let out = compare_sequence("t", "§x", &expected, &observed, "src");
        assert!(!out.passed);
        assert!(out.detail.contains("第 1 块"), "{}", out.detail);
    }

    #[test]
    fn a_block_count_mismatch_shows_what_the_parser_saw() {
        let expected = vec!["a".to_string()];
        let observed = vec!["a".to_string(), "b".to_string()];
        let out = compare_sequence("t", "§x", &expected, &observed, "src");
        assert!(!out.passed);
        assert!(out.detail.contains("块数不符"), "{}", out.detail);
    }

    #[test]
    fn tolerance_applies_per_component_of_a_box() {
        let a = [0.0, 0.0, 100.0, 100.0];
        let b = [0.0, 0.0, 100.0, 100.04];
        assert!(arrays_close(&a, &b, 0.05));
        assert!(!arrays_close(&a, &b, 0.01));
    }

    fn geom(key: &str, metric: [f64; 4]) -> BlockGeometry {
        BlockGeometry {
            key: key.into(),
            page: 0,
            metric_box: metric,
            visual_bbox: metric,
            baseline_origin: [metric[0], metric[1]],
        }
    }

    #[test]
    fn cross_fixture_geometry_absorbs_rounding_but_not_a_real_shift() {
        let a = [geom("L1", [10.0, 20.0, 30.0, 40.0])];
        let rounded = [geom("L1", [10.0, 20.0, 30.0, 40.02])];
        let shifted = [geom("L1", [10.0, 20.0, 30.0, 41.0])];
        assert!(geometry_differences(&a, &rounded, 0.05).is_empty());
        // 变的是上边沿，因此 metric_box 与 visual_bbox 各报一条，
        // baseline_origin（取左下角）不受影响。
        assert_eq!(geometry_differences(&a, &shifted, 0.05).len(), 2);
    }

    #[test]
    fn cross_fixture_geometry_reports_a_block_count_mismatch_first() {
        let a = [geom("L1", [0.0, 0.0, 1.0, 1.0])];
        let diffs = geometry_differences(&a, &[], 0.05);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("块数"), "{diffs:?}");
    }

    #[test]
    fn the_rebuild_gap_stays_long_enough_to_expose_a_wall_clock_leak() {
        assert!(determinism::DEFAULT_GAP >= std::time::Duration::from_millis(1000));
    }
}
