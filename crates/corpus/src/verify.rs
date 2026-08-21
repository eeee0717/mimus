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
    if manifest.requires(Check::ReadingOrder) {
        outcomes.push(check_reading_order(manifest, &poppler_pages));
    }

    let raster = if manifest.requires(Check::Render) {
        render::rasterize(&pdf, &fixture_work.join("raster"), manifest.page.len())?
    } else {
        Vec::new()
    };

    if manifest.requires(Check::DualParserGeometry) {
        outcomes.push(check_dual_parser_geometry(
            manifest,
            &mutool_pages,
            &poppler_pages,
            &raster,
            mode,
        )?);
    }
    if manifest.requires(Check::Render) && mode == Mode::Verify {
        outcomes.push(check_render(manifest, &raster)?);
    }

    let draw_order = mutool_pages
        .iter()
        .map(|p| p.blocks.iter().map(|b| b.text.clone()).collect())
        .collect();

    Ok(Report {
        fixture: manifest.id().to_string(),
        outcomes,
        draw_order,
        raster,
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

    // 第二个解析器：poppler 报的是**观看空间**尺寸，因此它同时校验了
    // manifest 的 /Rotate 与本工具的换算方向——两者任一错了这里就不符。
    let poppler_pages = poppler::blocks(pdf, frames)?;
    let mut size_problems = Vec::new();
    for (frame, page) in frames.iter().zip(&poppler_pages) {
        let (w, h) = frame.viewer_size();
        if !close(w, page.viewer_size.0, tol) || !close(h, page.viewer_size.1, tol) {
            size_problems.push(format!(
                "第 {} 页观看尺寸：manifest 推得 {w}×{h}，poppler 报 {}×{}",
                page.index, page.viewer_size.0, page.viewer_size.1
            ));
        }
    }
    outcomes.push(if size_problems.is_empty() {
        Outcome::ok(
            CHECK,
            "§2.3",
            "poppler 的观看尺寸与 manifest 的 /Rotate 推导一致",
        )
    } else {
        Outcome::fail(CHECK, "§2.3", size_problems.join("；"))
    });

    Ok(outcomes)
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
    blocks.iter().map(|b| text::normalize(&b.text)).collect()
}

fn flatten(pages: &[ParsedPage]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|p| p.blocks.iter().map(|b| b.text.clone()))
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
    raster: &[PageRaster],
    mode: Mode,
) -> Result<Outcome> {
    const CHECK: &str = "dual-parser-geom";
    const CLAUSE: &str = "§2.1";

    if manifest.expected.geometry_source != GeometrySource::DualParserAdjudicated {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            "本 fixture 声明的是手写几何，不应走双解析器裁定",
        ));
    }

    let mutool = flatten_blocks(mutool_pages);
    let poppler = flatten_blocks(poppler_pages);
    let tol = manifest.expected.tolerance_pt;

    let mut blocks = Vec::new();
    let mut problems = Vec::new();

    for block in &manifest.expected.block {
        let m = &mutool[block.draw_order - 1];
        let p = &poppler[block.reading_order - 1];

        // 两个解析器共同报告的量只有文本与 x 跨度——先确认它们说的是同一块。
        if m.text != p.text {
            problems.push(format!(
                "块 `{}`：两个解析器指向的不是同一块\n    mutool  {:?}\n    poppler {:?}",
                block.key, m.text, p.text
            ));
            continue;
        }
        if !close(m.rect.x0, p.rect.x0, tol) || !close(m.rect.x1, p.rect.x1, tol) {
            problems.push(format!(
                "块 `{}` 的 x 跨度不一致（容差 {tol} pt）：mutool [{:.4}, {:.4}]，poppler [{:.4}, {:.4}]",
                block.key, m.rect.x0, m.rect.x1, p.rect.x0, p.rect.x1
            ));
            continue;
        }
        // y 方向两者报的是不同的量（墨迹盒 vs 度量盒），要求相等是错的；
        // 可判定的关系是包含：墨迹必须落在度量盒内。
        if !m.rect.contained_in(p.rect, tol) {
            problems.push(format!(
                "块 `{}` 的墨迹盒越出度量盒：mutool {:?}，poppler {:?}",
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
        if baseline.y < p.rect.y0 - tol || baseline.y > p.rect.y1 + tol {
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
        // §2.1：「两者不一致即阻止该 fixture 入库」。这里不写 adjudicated.toml。
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "poppler 与 mutool 不一致，按 §2.1 阻止入库：\n{}",
                indent(&problems.join("\n"))
            ),
        ));
    }

    let fresh = Adjudicated {
        schema_version: crate::adjudicated::SUPPORTED_SCHEMA_VERSION,
        fixture: manifest.id().to_string(),
        tolerance_pt: tol,
        block: blocks,
        render: raster.iter().map(RenderReference::from).collect(),
    };

    let path = manifest.adjudicated_path();
    match mode {
        Mode::Adjudicate => {
            std::fs::write(&path, fresh.to_toml())
                .with_context(|| format!("写入 {} 失败", path.display()))?;
            Ok(Outcome::ok(
                CHECK,
                CLAUSE,
                format!(
                    "{} 块已由 poppler 与 mutool 一致裁定并写入 adjudicated.toml",
                    fresh.block.len()
                ),
            ))
        }
        Mode::Verify => {
            let recorded = Adjudicated::load(&path)?;
            let diffs = recorded.differences(&fresh);
            Ok(if diffs.is_empty() {
                Outcome::ok(
                    CHECK,
                    CLAUSE,
                    format!("{} 块与已记录的裁定结果一致", fresh.block.len()),
                )
            } else {
                Outcome::fail(
                    CHECK,
                    CLAUSE,
                    format!("与 adjudicated.toml 不符：\n{}", indent(&diffs.join("\n"))),
                )
            })
        }
    }
}

fn flatten_blocks(pages: &[ParsedPage]) -> Vec<crate::oracle::ParsedBlock> {
    pages.iter().flat_map(|p| p.blocks.clone()).collect()
}

// ---------------------------------------------------------------- §2.8 步骤 4

fn check_render(manifest: &Manifest, raster: &[PageRaster]) -> Result<Outcome> {
    const CHECK: &str = "render";
    const CLAUSE: &str = "§2.8";

    let recorded = Adjudicated::load(&manifest.adjudicated_path())?;
    let fresh: Vec<RenderReference> = raster.iter().map(RenderReference::from).collect();
    Ok(if recorded.render == fresh {
        Outcome::ok(
            CHECK,
            CLAUSE,
            format!(
                "{} 页在 poppler 与 mutool 下的栅格哈希均与参考一致",
                fresh.len()
            ),
        )
    } else {
        Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "栅格哈希与参考不符：\n    参考 {recorded:?}\n    本次 {fresh:?}",
                recorded = recorded.render
            ),
        )
    })
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

fn arrays_close(a: &[f64; 4], b: &[f64; 4], tol: f64) -> bool {
    a.iter().zip(b).all(|(x, y)| close(*x, *y, tol))
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("       {l}"))
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn the_rebuild_gap_stays_long_enough_to_expose_a_wall_clock_leak() {
        assert!(determinism::DEFAULT_GAP >= std::time::Duration::from_millis(1000));
    }
}
