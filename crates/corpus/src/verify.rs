//! `corpus verify` / `corpus adjudicate` —— §2.8 的五步独立验收。
//!
//! 失败信息一律带上 fixture ID 与合同条款号。语料的失败大多不是「代码有 bug」，
//! 而是「这份 fixture 违反了某条约定」——不指出是哪一条，排查就得从头读一遍
//! 合同。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::adjudicated::{Adjudicated, BlockGeometry, RenderReference};
use crate::determinism;
use crate::exact;
use crate::geom::{PageFrame, Rect, close};
use crate::hash;
use crate::manifest::{Check, GeometrySource, Legality, Manifest, Method};
use crate::mutation::{self, MutationSpec};
use crate::oracle::render::PageRaster;
use crate::oracle::{ParsedPage, mupdf, mupdf_svg, mupdf_trace, poppler, qpdf, render};
use crate::proc;
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

/// Audit committed acceptance evidence without rerunning version-sensitive
/// typesetters or renderers. Hosted CI uses this together with the production
/// all-fixture gate; `doctor` + `determinism` + `verify` remain the authority
/// whenever the exact pinned corpus toolchain is available.
pub fn audit_committed(manifests: &[Manifest], repo_root: &Path) -> Result<bool> {
    if manifests.is_empty() {
        println!("corpus/fixtures/ 下没有 fixture。");
        return Ok(true);
    }

    let all_manifests = discover(repo_root)?;
    let mut passed = true;
    for manifest in manifests {
        let mut outcomes = vec![
            check_lineage(manifest, &all_manifests),
            check_pins(manifest)?,
            check_committed_hash(manifest)?,
            check_recorded_adjudication(manifest)?,
        ];
        if manifest.requires(Check::Legality) && manifest.identity.legality == Legality::Legal {
            outcomes.push(check_legality(manifest, &manifest.pdf_path())?);
        }
        println!(
            "\n{}  [{:?}] {}",
            manifest.id(),
            manifest.identity.priority,
            manifest.identity.name
        );
        for outcome in &outcomes {
            print_outcome(outcome);
            passed &= outcome.passed;
        }
    }

    println!(
        "\n{} 份 fixture，{}",
        manifests.len(),
        if passed {
            "全部通过入库证据审计"
        } else {
            "存在未通过的入库证据"
        }
    );
    Ok(passed)
}

fn check_committed_hash(manifest: &Manifest) -> Result<Outcome> {
    let actual = hash::of_file(&manifest.pdf_path())?;
    Ok(if actual == manifest.source.pdf_sha256 {
        Outcome::ok(
            "committed-hash",
            "§2.6",
            format!("PDF SHA-256 匹配（{}）", &actual[..16]),
        )
    } else {
        Outcome::fail(
            "committed-hash",
            "§2.6",
            format!(
                "PDF SHA-256 不符：manifest {}，实际 {actual}",
                manifest.source.pdf_sha256
            ),
        )
    })
}

fn check_recorded_adjudication(manifest: &Manifest) -> Result<Outcome> {
    let required = manifest.requires(Check::DualParserGeometry) || manifest.requires(Check::Render);
    if !required {
        return Ok(Outcome::ok(
            "recorded-evidence",
            "§2.8",
            "该 fixture 由声明的非栅格 oracle 裁定",
        ));
    }

    let recorded = Adjudicated::load(&manifest.adjudicated_path())?;
    let mut problems = Vec::new();
    if recorded.fixture != manifest.id() {
        problems.push(format!("fixture 字段为 {:?}", recorded.fixture));
    }
    if manifest.requires(Check::DualParserGeometry)
        && recorded.block.len() != manifest.expected.block.len()
    {
        problems.push(format!(
            "裁定块数 {}，manifest 块数 {}",
            recorded.block.len(),
            manifest.expected.block.len()
        ));
    }
    if manifest.requires(Check::Render) {
        if recorded.render.len() != manifest.page.len() {
            problems.push(format!(
                "参考栅格页数 {}，manifest 页数 {}",
                recorded.render.len(),
                manifest.page.len()
            ));
        }
        for (index, render) in recorded.render.iter().enumerate() {
            if render.page != index || render.dpi != render::DPI {
                problems.push(format!(
                    "参考栅格 {index} 的 page/dpi 为 {}/{}",
                    render.page, render.dpi
                ));
            }
            if !is_lower_hex(&render.poppler_sha256, 64)
                || render
                    .poppler_linux_x86_64_sha256
                    .as_deref()
                    .is_some_and(|value| !is_lower_hex(value, 64))
                || !is_lower_hex(&render.mutool_md5, 32)
            {
                problems.push(format!("参考栅格 {index} 的哈希格式无效"));
            }
        }
    }

    Ok(if problems.is_empty() {
        Outcome::ok(
            "recorded-evidence",
            "§2.8",
            format!(
                "{} 块双解析器几何 + {} 页双渲染器哈希已记录",
                recorded.block.len(),
                recorded.render.len()
            ),
        )
    } else {
        Outcome::fail("recorded-evidence", "§2.8", problems.join("；"))
    })
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

    let all_manifests = discover(repo_root)?;
    let mut reports = Vec::new();
    for manifest in manifests {
        let mut report = verify_one(manifest, toolchain, repo_root, work_dir, mode)?;
        report
            .outcomes
            .insert(0, check_lineage(manifest, &all_manifests));
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

/// Add corpus-owned exact/mutation recipes to `corpus determinism`, alongside
/// the external-engine probes in `determinism::run`.
pub fn run_owned_determinism(
    manifests: &[Manifest],
    toolchain: &Toolchain,
    repo_root: &Path,
    work_dir: &Path,
) -> Result<bool> {
    let owned: Vec<&Manifest> = manifests
        .iter()
        .filter(|manifest| manifest.source.method != Method::RealisticTypesetting)
        .collect();
    if owned.is_empty() {
        return Ok(true);
    }
    println!("\nCorpus-owned fixture recipes:");
    let mut passed = true;
    for manifest in owned {
        let outcome = check_determinism(manifest, toolchain, repo_root, work_dir)?;
        print_outcome(&outcome);
        passed &= outcome.passed;
    }
    Ok(passed)
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
    let Some(parent) = all
        .iter()
        .find(|candidate| candidate.id() == lineage.parent)
    else {
        return Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "声明的合法父本 `{}` 不在本次验收的 fixture 集合里",
                lineage.parent
            ),
        );
    };
    if parent.identity.legality != Legality::Legal {
        return Outcome::fail(
            CHECK,
            CLAUSE,
            format!("父本 `{}` 本身不是 legal", parent.id()),
        );
    }

    let checked = (|| -> Result<String> {
        let mutation = lineage
            .mutations
            .first()
            .context("manifest schema should require exactly one mutation")?;
        let parent_bytes = std::fs::read(parent.pdf_path())?;
        let child_bytes = std::fs::read(manifest.pdf_path())?;
        let derived = mutation::derive(
            &parent_bytes,
            MutationSpec {
                parent_fixture_id: &lineage.parent,
                byte_offset: usize::try_from(mutation.byte_offset)
                    .context("mutation offset exceeds usize")?,
                expected_bytes: &mutation.original_bytes,
                replacement_bytes: &mutation.replacement_bytes,
                semantics: &mutation.description,
            },
        )?;
        mutation::verify(&parent_bytes, &child_bytes, &derived.record)?;
        if derived.bytes != child_bytes {
            bail!("child bytes do not equal the declared derivation");
        }
        Ok(format!(
            "父本 `{}`，唯一区间 {}..{}：{}",
            lineage.parent,
            mutation.byte_offset,
            mutation.byte_offset + mutation.original_bytes.len() as u64,
            mutation.description
        ))
    })();

    match checked {
        Ok(detail) => Outcome::ok(CHECK, CLAUSE, detail),
        Err(error) => Outcome::fail(CHECK, CLAUSE, format!("谱系字节核验失败：{error:#}")),
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

    let needs_text_oracles = [
        Check::Glyphs,
        Check::ReadingOrder,
        Check::DualParserGeometry,
        Check::HandWrittenGeometry,
        Check::Type3Geometry,
        Check::FontAdvance,
        Check::EmbeddedCmap,
    ]
    .into_iter()
    .any(|check| manifest.requires(check))
        || manifest.requires(Check::Structure);
    let needs_frames = manifest.requires(Check::PageGeometry)
        || needs_text_oracles
        || manifest.requires(Check::TransformedTextGeometry);
    let media_frames: Vec<PageFrame> = if needs_frames {
        manifest
            .page
            .iter()
            .map(|page| {
                let media_box = page
                    .numeric_media_box()
                    .context("non-numeric MediaBox has no coordinate frame")?;
                PageFrame::new(media_box, page.rotate)
            })
            .collect::<Result<_>>()
            .with_context(|| format!("[{}] 页面框无效", manifest.id()))?
    } else {
        Vec::new()
    };
    // MuPDF reports text quads relative to the effective viewing box, while
    // Poppler and pdftoppm report page dimensions/coordinates relative to the
    // MediaBox. Keep those parser-specific origins explicit so both signals
    // are converted into the same page-space contract.
    let needs_mutool_frames =
        needs_text_oracles || manifest.requires(Check::TransformedTextGeometry);
    let mutool_frames: Vec<PageFrame> = if needs_mutool_frames {
        manifest
            .page
            .iter()
            .map(|page| {
                PageFrame::new(
                    page.effective_box()
                        .context("page has no numeric effective box")?,
                    page.rotate,
                )
            })
            .collect::<Result<_>>()
            .with_context(|| format!("[{}] MuPDF 页面框无效", manifest.id()))?
    } else {
        Vec::new()
    };

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
    if manifest.requires(Check::OperatorWalk) {
        outcomes.push(check_operator_walk(manifest, repo_root)?);
    }
    // A deliberately broken object graph uses qpdf's declared failure as its
    // structure gate. Content-stream failures still run the operator walker
    // and independent text oracles requested by their manifests.
    let declares_parser_failure = manifest
        .expected
        .declared_failure
        .as_deref()
        .is_some_and(|failure| !failure.starts_with("content-semantics:"));
    let uses_parser_failure_structure = manifest.identity.legality == Legality::Malformed
        && manifest.requires(Check::Legality)
        && manifest.requires(Check::Structure)
        && !manifest.requires(Check::OperatorWalk)
        && declares_parser_failure;
    if uses_parser_failure_structure {
        outcomes.push(check_malformed_structure(manifest, &pdf)?);
    }
    if manifest.requires(Check::PageGeometry) {
        outcomes.extend(check_page_geometry(manifest, &pdf, &media_frames)?);
    }
    let qpdf_document =
        if manifest.requires(Check::PdfBytes) || manifest.requires(Check::PdfStructure) {
            Some(qpdf::Document::load(&pdf)?)
        } else {
            None
        };
    if manifest.requires(Check::PdfBytes) {
        outcomes.push(check_pdf_bytes(
            manifest,
            &pdf,
            qpdf_document.as_ref().context("qpdf document missing")?,
        )?);
    }
    if manifest.requires(Check::PdfStructure) {
        outcomes.push(check_pdf_structure(
            manifest,
            qpdf_document.as_ref().context("qpdf document missing")?,
        )?);
    }

    // 语义畸形 fixture 不应被一个未声明的文本解析器提前短路；只在对应门禁
    // 真正需要时调用两个文本/几何 oracle。
    let run_text_oracles = needs_text_oracles && !uses_parser_failure_structure;
    let (mutool_pages, poppler_pages) = if run_text_oracles {
        let mutool_pages = if manifest
            .expected
            .block
            .iter()
            .any(|block| block.mutool_extractable)
        {
            mupdf::blocks(&pdf, &mutool_frames)?
        } else {
            Vec::new()
        };
        (mutool_pages, poppler::blocks(&pdf, &media_frames)?)
    } else {
        (Vec::new(), Vec::new())
    };
    // A singular Form CTM is intentionally non-locatable for MuPDF. Poppler
    // may still extract that Form's text, so structure/glyph contracts compare
    // only declared page-level blocks for the XOBJ-08 fixture.
    let poppler_contract_pages = filter_nonlocatable_xobject_text(manifest, &poppler_pages);

    if manifest.requires(Check::Structure) && !uses_parser_failure_structure {
        outcomes.extend(check_structure(
            manifest,
            &mutool_pages,
            &poppler_contract_pages,
        ));
    }
    if manifest.requires(Check::Glyphs) {
        outcomes.extend(check_glyphs(
            manifest,
            &mutool_pages,
            &poppler_contract_pages,
        ));
    }
    if manifest.requires(Check::ReadingOrder) {
        outcomes.push(check_reading_order(manifest, &poppler_contract_pages));
    }
    if manifest.requires(Check::HandWrittenGeometry) {
        outcomes.extend(check_hand_written_geometry(
            manifest,
            &pdf,
            &mutool_frames,
            &mutool_pages,
            &poppler_pages,
        )?);
    }
    if manifest.requires(Check::TransformedTextGeometry) {
        outcomes.extend(check_transformed_text_geometry(
            manifest,
            &pdf,
            &mutool_frames,
        )?);
    }
    if manifest.requires(Check::Type3Geometry) {
        outcomes.extend(check_type3_geometry(manifest, &mutool_pages));
    }
    if manifest.requires(Check::FontAdvance) {
        outcomes.extend(check_font_advance(manifest, &mutool_pages));
    }
    if manifest.requires(Check::EmbeddedCmap) {
        outcomes.extend(check_embedded_cmap(manifest, &mutool_pages)?);
    }
    if manifest.requires(Check::RenderDiagnostic) {
        let expected = manifest
            .expected
            .renderer_diagnostic
            .as_deref()
            .context("render-diagnostic check missing expected.renderer_diagnostic")?;
        let diagnostic = render::diagnostic(&pdf)?;
        outcomes.push(if diagnostic.contains(expected) {
            Outcome::ok(
                "render-diagnostic",
                "§2.8",
                format!("mutool trace 以声明的方式诊断：{expected:?}"),
            )
        } else {
            Outcome::fail(
                "render-diagnostic",
                "§2.8",
                format!(
                    "声明 {expected:?}，mutool 实际输出：\n{}",
                    indent(&diagnostic)
                ),
            )
        });
    }

    let raster = if manifest.requires(Check::Render) {
        render::rasterize(&pdf, &fixture_work.join("raster"), manifest.page.len())?
    } else {
        Vec::new()
    };

    if manifest.requires(Check::Render) && manifest.requires(Check::PageGeometry) {
        outcomes.push(check_raster_orientation(
            &media_frames,
            &manifest.page,
            &raster,
        ));
    }

    // 双解析器几何裁定与参考栅格共用一份 adjudicated.toml：两者都是**测出来的**
    // 结果，与手写的 manifest 分开存放（§2.1）。精确 fixture 在这里记录空几何和
    // 渲染哈希；其三种手写几何由 hand-written-geometry 单独核验。
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

fn check_malformed_structure(manifest: &Manifest, pdf: &Path) -> Result<Outcome> {
    const CHECK: &str = "structure";
    const CLAUSE: &str = "§2.1/§2.9";
    let declared = manifest
        .expected
        .declared_failure
        .as_deref()
        .context("malformed fixture missing expected.declared_failure")?;
    let result = qpdf::check(pdf)?;
    let report = result.report.to_ascii_lowercase();
    let matches = match declared {
        "outline cycle" => report.contains("loop detected"),
        other => report.contains(&other.to_ascii_lowercase()),
    };
    Ok(if matches {
        Outcome::ok(
            CHECK,
            CLAUSE,
            format!("结构门禁观察到声明的失败：{declared:?}"),
        )
    } else {
        Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "结构门禁未观察到声明的失败 {declared:?}：{}",
                indent(&result.report)
            ),
        )
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
            let end = mutation.byte_offset + mutation.original_bytes.len() as u64;
            if end > size {
                problems.push(format!(
                    "变异区间 {}..{end} 超出 PDF 长度 {size}（描述：{}）",
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

    let hashes = match manifest.source.method {
        Method::ExactWriter => [
            hash::of_bytes(&exact::generate(manifest.id(), repo_root)?),
            hash::of_bytes(&exact::generate(manifest.id(), repo_root)?),
        ],
        Method::ByteMutation => [
            hash::of_bytes(&regenerate_mutation(manifest, repo_root)?),
            hash::of_bytes(&regenerate_mutation(manifest, repo_root)?),
        ],
        Method::ToolGeneratedCommitted => [committed.clone(), committed.clone()],
        Method::RealisticTypesetting => {
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
            [hashes.remove(0), hashes.remove(0)]
        }
    };

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

    let detail = if manifest.source.method == Method::ToolGeneratedCommitted {
        format!("一次性工具生成的入库二进制哈希匹配（{}）", &committed[..16])
    } else {
        format!(
            "重复生成 2 次一致，且与入库 PDF 相同（{}）",
            &committed[..16]
        )
    };
    Ok(Outcome::ok(CHECK, CLAUSE, detail))
}

fn regenerate_mutation(manifest: &Manifest, repo_root: &Path) -> Result<Vec<u8>> {
    let lineage = manifest
        .lineage
        .as_ref()
        .context("byte mutation missing lineage")?;
    let parent = discover(repo_root)?
        .into_iter()
        .find(|candidate| candidate.id() == lineage.parent)
        .with_context(|| format!("找不到合法父本 `{}`", lineage.parent))?;
    let mutation = lineage
        .mutations
        .first()
        .context("byte mutation missing mutation record")?;
    let parent_bytes = std::fs::read(parent.pdf_path())?;
    Ok(mutation::derive(
        &parent_bytes,
        MutationSpec {
            parent_fixture_id: &lineage.parent,
            byte_offset: usize::try_from(mutation.byte_offset)
                .context("mutation offset exceeds usize")?,
            expected_bytes: &mutation.original_bytes,
            replacement_bytes: &mutation.replacement_bytes,
            semantics: &mutation.description,
        },
    )?
    .bytes)
}

// ---------------------------------------------------------------- §2.8 步骤 2

fn check_legality(manifest: &Manifest, pdf: &Path) -> Result<Outcome> {
    const CHECK: &str = "legality";
    const CLAUSE: &str = "§2.8";

    // PARSE-11 is a semantic outline failure. qpdf reports it as a warning
    // (and exits non-zero) rather than as a syntax error, so match the explicit
    // loop diagnostic instead of requiring a generic `--check` failure string.
    if manifest.identity.legality == Legality::Malformed
        && manifest.expected.declared_failure.as_deref() == Some("outline cycle")
    {
        let declared = manifest
            .expected
            .declared_failure
            .as_deref()
            .context("outline-cycle fixture missing declared_failure")?;
        let result = qpdf::check(pdf)?;
        let report = result.report.to_ascii_lowercase();
        return Ok(if !result.passed && report.contains("loop detected") {
            Outcome::ok(CHECK, CLAUSE, format!("以声明的方式失败：{declared:?}"))
        } else {
            Outcome::fail(
                CHECK,
                CLAUSE,
                format!("outline cycle 未被 qpdf 以声明方式拒绝：{}", result.report),
            )
        });
    }

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
            // 两个前缀都表示「容器合法、畸形只在 content stream 里」。区别只在谁裁定：
            // `operator-walk:` 交给冻结的 M0 PoC，`content-semantics:` 交给 mimus-core
            // 的生产测试。qpdf 对这两类的期望是一样的——容器必须能装载。
            let content_semantic = declared
                .strip_prefix("operator-walk:")
                .or_else(|| declared.strip_prefix("content-semantics:"));
            if let Some(error_id) = content_semantic {
                let container_loaded =
                    result.passed || result.report.contains("operation succeeded with warnings");
                return Ok(if container_loaded && !error_id.is_empty() {
                    Outcome::ok(
                        CHECK,
                        CLAUSE,
                        format!("qpdf 完成容器检查；生产路径负责裁定 {error_id}"),
                    )
                } else if error_id.is_empty() {
                    Outcome::fail(CHECK, CLAUSE, "content 语义 failure ID 为空")
                } else {
                    Outcome::fail(
                        CHECK,
                        CLAUSE,
                        format!(
                            "content 语义 fixture 的容器不合法：\n{}",
                            indent(&result.report)
                        ),
                    )
                });
            }
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

fn check_operator_walk(manifest: &Manifest, repo_root: &Path) -> Result<Outcome> {
    const CHECK: &str = "operator-walk";
    const CLAUSE: &str = "§2.8";

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let args = vec![
        "run".to_string(),
        "--quiet".to_string(),
        "-p".to_string(),
        "m0-experiment-2".to_string(),
        "--".to_string(),
        manifest.id().to_string(),
        "--repo-root".to_string(),
        repo_root.display().to_string(),
    ];
    let Some(output) = proc::run(&cargo, &args, repo_root, &BTreeMap::new())? else {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!("找不到 cargo：{cargo}"),
        ));
    };
    if !output.success() {
        return Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!("走查器退出失败：\n{}", indent(&output.diagnostics())),
        ));
    }
    let report: Value =
        serde_json::from_slice(&output.stdout).context("解析 operator-walk JSON")?;
    let comparison = &report["manifest"];
    let mut checks = vec!["diagnostic_matches"];
    if manifest.identity.legality == Legality::Legal || !manifest.expected.block.is_empty() {
        checks.push("text_matches");
    }
    if !manifest.expected.cid_sequence.is_empty() {
        checks.push("cid_sequence_matches");
    }
    let failed = checks
        .into_iter()
        .filter(|field| comparison[*field].as_bool() != Some(true))
        .collect::<Vec<_>>();
    let errors = report["errors"]
        .as_array()
        .context("operator-walk errors is not an array")?;
    let legal_errors = manifest.identity.legality == Legality::Legal && !errors.is_empty();
    let baseline_failed = comparison["expected_baseline"].is_array()
        && comparison["baseline_delta"].as_array().is_none_or(|delta| {
            delta.iter().any(|value| {
                value
                    .as_f64()
                    .is_none_or(|value| value.abs() > manifest.expected.tolerance_pt)
            })
        });

    if failed.is_empty() && !legal_errors && !baseline_failed {
        let diagnostics = comparison["observed_diagnostics"]
            .as_array()
            .context("operator-walk observed_diagnostics is not an array")?;
        Ok(Outcome::ok(
            CHECK,
            CLAUSE,
            format!("走查 JSON 与 manifest 一致；诊断 {diagnostics:?}"),
        ))
    } else {
        Ok(Outcome::fail(
            CHECK,
            CLAUSE,
            format!(
                "走查 JSON 不符合 manifest：failed={failed:?}, legal_errors={legal_errors}, baseline_failed={baseline_failed}; report={comparison}"
            ),
        ))
    }
}

// ---------------------------------------------------------------- 精确 PDF 字节与对象合同

fn check_pdf_bytes(manifest: &Manifest, pdf: &Path, document: &qpdf::Document) -> Result<Outcome> {
    const CHECK: &str = "pdf-bytes";
    const CLAUSE: &str = "§2.5/§2.6";

    let contract = manifest
        .expected
        .pdf
        .as_ref()
        .context("pdf-bytes check requires expected.pdf")?;
    let bytes = std::fs::read(pdf)?;
    let mut problems = Vec::new();

    let prefix = decode_hex(&contract.header_prefix_hex)?;
    if !bytes.starts_with(&prefix) {
        problems.push(format!(
            "输入前缀不符：期望 {}，实际 {}",
            contract.header_prefix_hex,
            encode_hex(&bytes[..bytes.len().min(prefix.len())])
        ));
    }
    if document.pdf_version()? != contract.version {
        problems.push(format!(
            "PDF version：manifest {}，qpdf {}",
            contract.version,
            document.pdf_version()?
        ));
    }
    let actual_objects = document.object_numbers()?;
    if actual_objects != contract.object_numbers {
        problems.push(format!(
            "对象号：manifest {:?}，qpdf {:?}",
            contract.object_numbers, actual_objects
        ));
    }
    let xref_offsets = qpdf::xref_offsets(pdf)?;
    problems.extend(object_plan_problems(
        &bytes,
        &contract.object_numbers,
        &xref_offsets,
        contract.xref_kind == crate::manifest::XrefKind::Table,
    ));
    match contract.xref_kind {
        crate::manifest::XrefKind::Table => {
            if find_bytes(&bytes, b"\nxref\n").is_none()
                || find_bytes(&bytes, b"\ntrailer\n").is_none()
            {
                problems.push("缺少 classic xref/trailer".to_string());
            }
        }
        crate::manifest::XrefKind::Stream => {
            if find_bytes(&bytes, b"/Type /XRef").is_none() {
                problems.push("缺少 /Type /XRef stream".to_string());
            }
        }
    }
    if document.trailer_reference("/Root")? != contract.root_object {
        problems.push(format!("trailer /Root 不是 {} 0 R", contract.root_object));
    }
    let expected_size = u64::from(
        contract
            .xref_size
            .unwrap_or_else(|| contract.object_numbers.len() as u32 + 1),
    );
    if document.trailer()?.get("/Size").and_then(Value::as_u64) != Some(expected_size) {
        problems.push(format!("trailer /Size 不是 {expected_size}"));
    }

    let expected_id = decode_hex(&contract.trailer_id_hex)?;
    let ids = document.trailer()?.get("/ID").and_then(Value::as_array);
    let ids_match = ids.is_some_and(|values| {
        values.len() == 2
            && values.iter().all(|value| {
                value
                    .as_str()
                    .and_then(|text| text.strip_prefix("u:"))
                    .is_some_and(|text| text.as_bytes() == expected_id)
            })
    });
    if !ids_match {
        problems.push(format!("trailer /ID 不是两份 {}", contract.trailer_id_hex));
    }
    let has_info = document.trailer()?.contains_key("/Info");
    if has_info != contract.info_dictionary {
        problems.push(format!(
            "Info 字典有无：manifest {}，qpdf {has_info}",
            contract.info_dictionary
        ));
    }
    let metadata = document.metadata_streams()?;
    if metadata != contract.metadata_streams {
        problems.push(format!(
            "metadata stream 数：manifest {}，qpdf {metadata}",
            contract.metadata_streams
        ));
    }

    for reference in &manifest.expected.reference {
        let expected_generation = reference.to_generation.unwrap_or(0);
        match document.reference_with_generation(reference.from_object, &reference.path) {
            Ok((actual, generation))
                if actual == reference.to_object && generation == expected_generation => {}
            Ok((actual, generation)) => problems.push(format!(
                "{}:{} -> {} {generation} R，期望 {} {expected_generation} R",
                reference.from_object,
                reference.path.join("/"),
                actual,
                reference.to_object
            )),
            Err(error) => problems.push(format!(
                "{}:{} 无法解析：{error:#}",
                reference.from_object,
                reference.path.join("/")
            )),
        }
    }
    for font in &manifest.source.fonts {
        let Some(object) = font.pdf_object else {
            continue;
        };
        let descriptor = font
            .descriptor_object
            .context("exact font missing descriptor_object")?;
        let subset_tag = font
            .subset_tag
            .as_deref()
            .context("exact font missing subset_tag")?;
        let base_name = font
            .base_name
            .as_deref()
            .context("exact font missing base_name")?;
        let expected_name = format!("/{subset_tag}+{base_name}");
        let dictionary = document.dictionary(object)?;
        if dictionary.get("/BaseFont").and_then(Value::as_str) != Some(&expected_name) {
            problems.push(format!(
                "font object {object} /BaseFont 不是 {expected_name}"
            ));
        }
        // Type0 fonts carry the descriptor on their first CIDFont descendant;
        // simple Type1/TrueType fonts carry it directly.
        let descriptor_reference =
            match document.reference(object, &["/FontDescriptor".to_string()]) {
                Ok(actual) => Ok(actual),
                Err(direct_error) => {
                    let descendants =
                        document.optional_references(object, &["/DescendantFonts".to_string()])?;
                    match descendants.first().copied() {
                        Some(descendant) => {
                            document.reference(descendant, &["/FontDescriptor".to_string()])
                        }
                        None => Err(direct_error),
                    }
                }
            };
        match descriptor_reference {
            Ok(actual) if actual == descriptor => {}
            Ok(actual) => problems.push(format!(
                "font object {object} /FontDescriptor 是 {actual} 0 R，期望 {descriptor} 0 R"
            )),
            Err(error) => problems.push(format!(
                "font object {object} /FontDescriptor 无法解析：{error:#}"
            )),
        }
        if document
            .dictionary(descriptor)?
            .get("/FontName")
            .and_then(Value::as_str)
            != Some(&expected_name)
        {
            problems.push(format!(
                "font descriptor {descriptor} /FontName 不是 {expected_name}"
            ));
        }
    }
    for stream in &manifest.expected.content_stream {
        let dictionary = document.stream_dictionary(stream.object)?;
        let compressed = dictionary.contains_key("/Filter");
        if compressed != stream.compressed {
            problems.push(format!(
                "content object {} 压缩状态：manifest {}，qpdf {compressed}",
                stream.object, stream.compressed
            ));
        }
        let actual = qpdf::raw_stream(pdf, stream.object)?;
        let expected_raw = if let Some(hex) = &stream.bytes_hex {
            decode_hex(hex)?
        } else {
            stream.bytes.as_bytes().to_vec()
        };
        if actual != expected_raw {
            problems.push(format!(
                "content object {} raw 字节不符：manifest {} bytes，qpdf {} bytes",
                stream.object,
                expected_raw.len(),
                actual.len()
            ));
        }
        if stream.compressed {
            let expected_decoded = if let Some(hex) = &stream.decoded_bytes_hex {
                decode_hex(hex)?
            } else {
                stream
                    .decoded_bytes
                    .as_deref()
                    .context("filtered stream missing decoded bytes")?
                    .as_bytes()
                    .to_vec()
            };
            let decoded = qpdf::filtered_stream(pdf, stream.object)?;
            if decoded != expected_decoded {
                problems.push(format!(
                    "content object {} decoded 字节不符：manifest {} bytes，qpdf {} bytes",
                    stream.object,
                    expected_decoded.len(),
                    decoded.len()
                ));
            }
            let observed = document.stream_filters(stream.object)?;
            if observed != stream.filters {
                problems.push(format!(
                    "content object {} filters：manifest {:?}，qpdf {:?}",
                    stream.object, stream.filters, observed
                ));
            }
        }
    }

    Ok(if problems.is_empty() {
        Outcome::ok(
            CHECK,
            CLAUSE,
            format!(
                "{} 个对象顺序、xref/trailer、{} 条引用、{} 个字体溯源、{} 个未压缩 content stream 均与手写合同一致",
                contract.object_numbers.len(),
                manifest.expected.reference.len(),
                manifest.source.fonts.len(),
                manifest.expected.content_stream.len()
            ),
        )
    } else {
        Outcome::fail(CHECK, CLAUSE, problems.join("；"))
    })
}

fn check_pdf_structure(manifest: &Manifest, document: &qpdf::Document) -> Result<Outcome> {
    use crate::manifest::{AnnotationSubtype, BookmarkTarget};

    const CHECK: &str = "pdf-structure";
    const CLAUSE: &str = "§2.7/§2.9";
    let tolerance = manifest.expected.tolerance_pt;
    let mut problems = Vec::new();
    let contract = manifest
        .expected
        .pdf
        .as_ref()
        .context("pdf-structure check requires expected.pdf")?;

    let outline_root =
        document.optional_reference(contract.root_object, &["/Outlines".to_string()])?;
    let observed_outline = match outline_root {
        Some(root) => walk_outline(document, root)?,
        None => Vec::new(),
    };
    let expected_outline: Vec<u32> = manifest
        .expected
        .bookmark
        .iter()
        .map(|bookmark| bookmark.object)
        .collect();
    compare_object_set(
        "bookmark objects",
        &expected_outline,
        observed_outline
            .iter()
            .map(|bookmark| bookmark.object)
            .collect(),
        &mut problems,
    );
    for observed in &observed_outline {
        let Some(expected) = manifest
            .expected
            .bookmark
            .iter()
            .find(|bookmark| bookmark.object == observed.object)
        else {
            continue;
        };
        if observed.parent != expected.parent_object || observed.level != expected.level {
            problems.push(format!(
                "bookmark {} hierarchy: qpdf parent/level {}/{}, manifest {}/{}",
                observed.object,
                observed.parent,
                observed.level,
                expected.parent_object,
                expected.level
            ));
        }
    }
    let observed_depth = observed_outline
        .iter()
        .map(|bookmark| bookmark.level)
        .max()
        .unwrap_or(0);
    if observed_depth != manifest.expected.structure.bookmark_depth {
        problems.push(format!(
            "bookmark depth: qpdf {observed_depth}, manifest {}",
            manifest.expected.structure.bookmark_depth
        ));
    }

    let mut actual_annotations = Vec::new();
    for page in document.page_objects()? {
        actual_annotations.extend(document.optional_references(page, &["/Annots".to_string()])?);
    }
    let expected_annotations: Vec<u32> = manifest
        .expected
        .annotation
        .iter()
        .map(|annotation| annotation.object)
        .collect();
    compare_object_set(
        "annotations",
        &expected_annotations,
        actual_annotations,
        &mut problems,
    );

    let field_roots = document.optional_references(
        contract.root_object,
        &["/AcroForm".to_string(), "/Fields".to_string()],
    )?;
    let actual_fields = walk_reference_tree(document, &field_roots, "/Kids")?;
    let expected_fields: Vec<u32> = manifest
        .expected
        .annotation
        .iter()
        .filter(|annotation| annotation.subtype == AnnotationSubtype::Widget)
        .map(|annotation| annotation.object)
        .collect();
    compare_object_set(
        "form fields",
        &expected_fields,
        actual_fields,
        &mut problems,
    );

    let actual_ocgs = document.optional_references(
        contract.root_object,
        &["/OCProperties".to_string(), "/OCGs".to_string()],
    )?;
    let expected_ocgs: Vec<u32> = manifest
        .expected
        .optional_content_group
        .iter()
        .map(|group| group.object)
        .collect();
    compare_object_set("OCGs", &expected_ocgs, actual_ocgs, &mut problems);
    let actual_ocg_order = document.optional_references(
        contract.root_object,
        &[
            "/OCProperties".to_string(),
            "/D".to_string(),
            "/Order".to_string(),
        ],
    )?;
    compare_object_set("OCG order", &expected_ocgs, actual_ocg_order, &mut problems);
    let expected_visible_ocgs: Vec<u32> = manifest
        .expected
        .optional_content_group
        .iter()
        .filter(|group| group.initially_visible)
        .map(|group| group.object)
        .collect();
    let actual_visible_ocgs = document.optional_references(
        contract.root_object,
        &[
            "/OCProperties".to_string(),
            "/D".to_string(),
            "/ON".to_string(),
        ],
    )?;
    compare_object_set(
        "visible OCGs",
        &expected_visible_ocgs,
        actual_visible_ocgs,
        &mut problems,
    );

    for bookmark in &manifest.expected.bookmark {
        let dictionary = document.dictionary(bookmark.object)?;
        if dictionary.get("/Title").and_then(pdf_text) != Some(bookmark.title.as_str()) {
            problems.push(format!("bookmark {} title 不符", bookmark.object));
        }
        if document.reference(bookmark.object, &["/Parent".to_string()])? != bookmark.parent_object
        {
            problems.push(format!("bookmark {} parent 不符", bookmark.object));
        }
        let count = dictionary.get("/Count").and_then(Value::as_i64);
        if count != bookmark.count.map(i64::from) {
            problems.push(format!(
                "bookmark {} /Count：manifest {:?}，qpdf {count:?}",
                bookmark.object, bookmark.count
            ));
        }
        let flags = dictionary.get("/F").and_then(Value::as_u64).unwrap_or(0);
        if flags != u64::from(bookmark.style_flags) {
            problems.push(format!(
                "bookmark {} /F：manifest {}，qpdf {flags}",
                bookmark.object, bookmark.style_flags
            ));
        }
        match (bookmark.color, dictionary.get("/C")) {
            (Some(expected), Some(actual))
                if numeric_array(actual, 3)?
                    .iter()
                    .zip(expected)
                    .all(|(left, right)| close(*left, right, tolerance)) => {}
            (None, None) => {}
            (expected, actual) => problems.push(format!(
                "bookmark {} /C：manifest {expected:?}，qpdf {actual:?}",
                bookmark.object
            )),
        }

        match bookmark.target {
            BookmarkTarget::Xyz => {
                let destination = dictionary
                    .get("/Dest")
                    .context("XYZ bookmark missing /Dest")?;
                if !destination_matches(
                    destination,
                    bookmark.page_object.context("XYZ bookmark missing page")?,
                    bookmark.xyz.context("XYZ bookmark missing coordinates")?,
                    tolerance,
                )? {
                    problems.push(format!("bookmark {} XYZ destination 不符", bookmark.object));
                }
            }
            BookmarkTarget::GotoXyz => {
                let action = dictionary
                    .get("/A")
                    .and_then(Value::as_object)
                    .context("GoTo bookmark missing /A")?;
                if action.get("/S").and_then(Value::as_str) != Some("/GoTo")
                    || !destination_matches(
                        action.get("/D").context("GoTo bookmark missing /D")?,
                        bookmark.page_object.context("GoTo bookmark missing page")?,
                        bookmark.xyz.context("GoTo bookmark missing coordinates")?,
                        tolerance,
                    )?
                {
                    problems.push(format!(
                        "bookmark {} GoTo destination 不符",
                        bookmark.object
                    ));
                }
            }
            BookmarkTarget::Named => {
                if dictionary.get("/Dest").and_then(pdf_text) != bookmark.name.as_deref() {
                    problems.push(format!(
                        "bookmark {} named destination 不符",
                        bookmark.object
                    ));
                }
            }
            BookmarkTarget::Uri => {
                let action = dictionary
                    .get("/A")
                    .and_then(Value::as_object)
                    .context("URI bookmark missing /A")?;
                if action.get("/S").and_then(Value::as_str) != Some("/URI")
                    || action.get("/URI").and_then(pdf_text) != bookmark.uri.as_deref()
                {
                    problems.push(format!("bookmark {} URI action 不符", bookmark.object));
                }
            }
        }
    }

    for annotation in &manifest.expected.annotation {
        let dictionary = document.dictionary(annotation.object)?;
        let expected_subtype = match annotation.subtype {
            AnnotationSubtype::Link => "/Link",
            AnnotationSubtype::Text => "/Text",
            AnnotationSubtype::Widget => "/Widget",
        };
        if dictionary.get("/Subtype").and_then(Value::as_str) != Some(expected_subtype) {
            problems.push(format!("annotation {} subtype 不符", annotation.object));
        }
        let rect = dictionary
            .get("/Rect")
            .context("annotation missing /Rect")?;
        if !numeric_array(rect, 4)?
            .iter()
            .zip(annotation.rect)
            .all(|(left, right)| close(*left, right, tolerance))
        {
            problems.push(format!("annotation {} rect 不符", annotation.object));
        }
        if let Some(expected_uri) = &annotation.uri {
            let action = dictionary
                .get("/A")
                .and_then(Value::as_object)
                .context("link annotation missing /A")?;
            if action.get("/S").and_then(Value::as_str) != Some("/URI")
                || action.get("/URI").and_then(pdf_text) != Some(expected_uri)
            {
                problems.push(format!("annotation {} URI 不符", annotation.object));
            }
        }
        if dictionary.get("/Contents").and_then(pdf_text) != annotation.contents.as_deref() {
            problems.push(format!("annotation {} contents 不符", annotation.object));
        }
        if let Some(field_name) = &annotation.field_name
            && (dictionary.get("/T").and_then(pdf_text) != Some(field_name)
                || dictionary.get("/FT").and_then(Value::as_str) != Some("/Tx"))
        {
            problems.push(format!("widget {} field name/type 不符", annotation.object));
        }
    }

    {
        let tree = document.optional_reference(
            contract.root_object,
            &["/Names".to_string(), "/Dests".to_string()],
        )?;
        let names: &[Value] = match tree {
            Some(tree) => document
                .value(tree, &["/Names".to_string()])?
                .as_array()
                .context("destination name tree /Names is not an array")?,
            None => &[],
        };
        if !names.len().is_multiple_of(2) {
            problems.push(format!(
                "destination name tree has odd /Names length {}",
                names.len()
            ));
        }
        let expected_names: Vec<String> = manifest
            .expected
            .named_destination
            .iter()
            .map(|destination| destination.name.clone())
            .collect();
        let actual_names: Vec<String> = names
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|pair| pdf_text(&pair[0]).map(str::to_string))
            .collect();
        compare_string_set(
            "named destinations",
            &expected_names,
            actual_names,
            &mut problems,
        );
        for expected in &manifest.expected.named_destination {
            let mut found = false;
            for pair in names.as_chunks::<2>().0 {
                if pdf_text(&pair[0]) == Some(expected.name.as_str()) {
                    found = destination_matches(
                        &pair[1],
                        expected.page_object,
                        expected.xyz,
                        tolerance,
                    )?;
                    break;
                }
            }
            if !found {
                problems.push(format!("named destination {:?} 不符", expected.name));
            }
        }
    }

    for group in &manifest.expected.optional_content_group {
        let dictionary = document.dictionary(group.object)?;
        if dictionary.get("/Type").and_then(Value::as_str) != Some("/OCG")
            || dictionary.get("/Name").and_then(pdf_text) != Some(group.name.as_str())
        {
            problems.push(format!("OCG {} type/name 不符", group.object));
        }
        let on = document
            .value(
                contract.root_object,
                &[
                    "/OCProperties".to_string(),
                    "/D".to_string(),
                    "/ON".to_string(),
                ],
            )?
            .as_array()
            .context("catalog OCProperties /ON is not an array")?;
        let visible = on
            .iter()
            .any(|value| reference_number(value) == Some(group.object));
        if visible != group.initially_visible {
            problems.push(format!("OCG {} initial visibility 不符", group.object));
        }
        let resource_key = format!("/{}", group.resource_name);
        if !manifest.expected.reference.iter().any(|reference| {
            reference.to_object == group.object && reference.path.last() == Some(&resource_key)
        }) {
            problems.push(format!(
                "OCG {} 没有通过 resource name /{} 引用",
                group.object, group.resource_name
            ));
        }
    }

    let actual_uri_actions = document.uri_action_count()?;
    if actual_uri_actions != manifest.expected.structure.uri_actions {
        problems.push(format!(
            "URI actions: qpdf {actual_uri_actions}, manifest {}",
            manifest.expected.structure.uri_actions
        ));
    }

    Ok(if problems.is_empty() {
        Outcome::ok(
            CHECK,
            CLAUSE,
            format!(
                "{} 层/{} 项书签、{} 个注释、{} 个表单域、{} 个 OCG、{} 个命名目标、{} 个 URI action 均精确匹配",
                manifest.expected.structure.bookmark_depth,
                manifest.expected.structure.bookmarks,
                manifest.expected.structure.annotations,
                manifest.expected.structure.form_fields,
                manifest.expected.structure.optional_content_groups,
                manifest.expected.structure.named_destinations,
                manifest.expected.structure.uri_actions
            ),
        )
    } else {
        Outcome::fail(CHECK, CLAUSE, problems.join("；"))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservedBookmark {
    object: u32,
    parent: u32,
    level: usize,
}

fn walk_outline(document: &qpdf::Document, root: u32) -> Result<Vec<ObservedBookmark>> {
    let mut observed = Vec::new();
    let mut seen = BTreeSet::new();
    walk_outline_children(document, root, 1, &mut seen, &mut observed)?;
    Ok(observed)
}

fn walk_outline_children(
    document: &qpdf::Document,
    parent: u32,
    level: usize,
    seen: &mut BTreeSet<u32>,
    observed: &mut Vec<ObservedBookmark>,
) -> Result<()> {
    let dictionary = document.dictionary(parent)?;
    let first = optional_reference(dictionary.get("/First"))?;
    let last = optional_reference(dictionary.get("/Last"))?;
    if first.is_none() != last.is_none() {
        bail!("outline parent {parent} must have both /First and /Last or neither");
    }

    let mut current = first;
    let mut previous = None;
    while let Some(object) = current {
        if !seen.insert(object) {
            bail!("outline cycle or duplicate object {object}");
        }
        let child = document.dictionary(object)?;
        if optional_reference(child.get("/Parent"))? != Some(parent) {
            bail!("outline object {object} has the wrong /Parent");
        }
        if optional_reference(child.get("/Prev"))? != previous {
            bail!("outline object {object} has the wrong /Prev sibling");
        }
        observed.push(ObservedBookmark {
            object,
            parent,
            level,
        });
        walk_outline_children(document, object, level + 1, seen, observed)?;
        previous = Some(object);
        current = optional_reference(child.get("/Next"))?;
    }
    if previous != last {
        bail!(
            "outline parent {parent} /Last is {:?}, traversed {:?}",
            last,
            previous
        );
    }
    Ok(())
}

fn optional_reference(value: Option<&Value>) -> Result<Option<u32>> {
    value
        .map(|value| reference_number(value).context("outline value is not an indirect reference"))
        .transpose()
}

fn walk_reference_tree(
    document: &qpdf::Document,
    roots: &[u32],
    kids_key: &str,
) -> Result<Vec<u32>> {
    fn visit(
        document: &qpdf::Document,
        object: u32,
        kids_key: &str,
        seen: &mut BTreeSet<u32>,
        observed: &mut Vec<u32>,
    ) -> Result<()> {
        if !seen.insert(object) {
            bail!("reference tree cycle or duplicate object {object}");
        }
        let kids = document.optional_references(object, &[kids_key.to_string()])?;
        if kids.is_empty() {
            observed.push(object);
        } else {
            for kid in kids {
                visit(document, kid, kids_key, seen, observed)?;
            }
        }
        Ok(())
    }

    let mut seen = BTreeSet::new();
    let mut observed = Vec::new();
    for root in roots {
        visit(document, *root, kids_key, &mut seen, &mut observed)?;
    }
    Ok(observed)
}

fn compare_object_set(
    label: &str,
    expected: &[u32],
    mut actual: Vec<u32>,
    problems: &mut Vec<String>,
) {
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    actual.sort_unstable();
    if actual != expected {
        problems.push(format!(
            "{label}: qpdf objects {actual:?}, manifest {expected:?}"
        ));
    }
}

fn compare_string_set(
    label: &str,
    expected: &[String],
    mut actual: Vec<String>,
    problems: &mut Vec<String>,
) {
    let mut expected = expected.to_vec();
    expected.sort();
    actual.sort();
    if actual != expected {
        problems.push(format!("{label}: qpdf {actual:?}, manifest {expected:?}"));
    }
}

fn pdf_text(value: &Value) -> Option<&str> {
    value.as_str().and_then(|text| text.strip_prefix("u:"))
}

fn numeric_array(value: &Value, length: usize) -> Result<Vec<f64>> {
    let array = value.as_array().context("expected a numeric array")?;
    if array.len() != length {
        bail!(
            "numeric array has length {}, expected {length}",
            array.len()
        );
    }
    array
        .iter()
        .map(|number| number.as_f64().context("array member is not numeric"))
        .collect()
}

fn destination_matches(
    value: &Value,
    page_object: u32,
    xyz: [f64; 3],
    tolerance: f64,
) -> Result<bool> {
    let array = value.as_array().context("destination is not an array")?;
    if array.len() != 5
        || reference_number(&array[0]) != Some(page_object)
        || array[1].as_str() != Some("/XYZ")
    {
        return Ok(false);
    }
    Ok(array[2..].iter().zip(xyz).all(|(actual, expected)| {
        actual
            .as_f64()
            .is_some_and(|value| close(value, expected, tolerance))
    }))
}

fn reference_number(value: &Value) -> Option<u32> {
    let mut parts = value.as_str()?.split_whitespace();
    let number = parts.next()?.parse().ok()?;
    let generation = parts.next()?;
    (generation.parse::<u16>().is_ok() && parts.next() == Some("R") && parts.next().is_none())
        .then_some(number)
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        bail!("odd-length hex string");
    }
    (0..text.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&text[index..index + 2], 16)
                .with_context(|| format!("invalid hex at byte {index}"))
        })
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn object_plan_problems(
    bytes: &[u8],
    object_numbers: &[u32],
    xref_offsets: &BTreeMap<u32, usize>,
    require_every_object_uncompressed: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut positions = Vec::new();
    for object in object_numbers {
        let Some(position) = xref_offsets.get(object).copied() else {
            if require_every_object_uncompressed {
                problems.push(format!("xref 中找不到 uncompressed object {object}"));
            }
            continue;
        };
        positions.push(position);
        let object_prefix = format!("{object} ");
        let valid_header = bytes.get(position..).is_some_and(|tail| {
            tail.starts_with(object_prefix.as_bytes())
                && tail
                    .windows(b" obj\n".len())
                    .position(|window| window == b" obj\n")
                    .is_some_and(|end| end < 32)
        });
        if !valid_header {
            problems.push(format!(
                "xref object {object} offset {position} 不指向精确对象头"
            ));
        }
    }
    if positions.windows(2).any(|pair| pair[0] >= pair[1]) {
        problems.push("对象写出顺序与 object_numbers 不一致".to_string());
    }
    problems
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
    let qpdf_document = qpdf::Document::load(pdf)?;
    let page_objects = qpdf_document.page_objects()?;
    let mut problems = Vec::new();
    for ((page, seen), page_object) in manifest.page.iter().zip(&observed).zip(page_objects) {
        let Some(media_box) = page.numeric_media_box() else {
            problems.push(format!("第 {} 页 MediaBox 含非数值分量", page.index));
            continue;
        };
        let inherited_media = qpdf_document.inherited_numeric_array(page_object, "/MediaBox")?;
        if inherited_media.as_deref() != Some(media_box.as_slice()) {
            problems.push(format!(
                "第 {} 页 qpdf 继承 MediaBox：manifest {:?}，qpdf {:?}",
                page.index, media_box, inherited_media
            ));
        }
        if !arrays_close(&media_box, &seen.media_box, tol) {
            problems.push(format!(
                "第 {} 页 MediaBox：manifest {:?}，mutool {:?}",
                page.index, media_box, seen.media_box
            ));
        }
        let inherited_crop = qpdf_document.inherited_numeric_array(page_object, "/CropBox")?;
        if inherited_crop.as_deref() != page.crop_box.as_ref().map(<[f64; 4]>::as_slice) {
            problems.push(format!(
                "第 {} 页 qpdf 继承 CropBox：manifest {:?}，qpdf {:?}",
                page.index, page.crop_box, inherited_crop
            ));
        }
        match (page.crop_box, seen.crop_box) {
            (Some(a), Some(b)) if !arrays_close(&a, &b, tol) => problems.push(format!(
                "第 {} 页 CropBox：manifest {a:?}，mutool {b:?}",
                page.index
            )),
            // `mutool pages` exposes only a local CropBox. qpdf's object tree
            // above and MuPDF's effective stext frame independently cover an
            // inherited value, so absence here is not a contradiction.
            (Some(_), None) if inherited_crop.is_some() => {}
            (a, b) if a.is_some() != b.is_some() => {
                problems.push(format!(
                    "第 {} 页 CropBox 有无不一致：manifest {a:?}，mutool {b:?}",
                    page.index
                ));
            }
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

    // 第二个解析器：poppler 独立报出 MediaBox 的**尺寸**。它不覆盖 /Rotate——
    // 见 `PageFrame::box_size` 的实测记录：`-bbox-layout` 的 <page width height>
    // 不应用 /Rotate，而同一份输出里的坐标应用了。这里严格比较这一已知量；
    // 观看空间中 /Rotate 的宽高交换由下方的 raster-orientation 单独断言。
    let poppler_pages = poppler::blocks(pdf, frames)?;
    let mut size_problems = Vec::new();
    for ((frame, expected_page), page) in frames.iter().zip(&manifest.page).zip(&poppler_pages) {
        let (w, h) = frame.box_size();
        let media_box = expected_page
            .numeric_media_box()
            .context("page-geometry check requires numeric MediaBox")?;
        let media_w = media_box[2] - media_box[0];
        let media_h = media_box[3] - media_box[1];
        // Poppler's <page width height> is defined by MediaBox even when a
        // CropBox is present (and does not apply /Rotate). Keep that measured
        // behaviour explicit rather than rejecting a valid non-zero CropBox.
        if !close(media_w, page.viewer_size.0, tol) || !close(media_h, page.viewer_size.1, tol) {
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
fn check_raster_orientation(
    frames: &[PageFrame],
    expected_pages: &[crate::manifest::Page],
    raster: &[PageRaster],
) -> Outcome {
    const CHECK: &str = "raster-orientation";
    const CLAUSE: &str = "§2.3";

    // pdftoppm 按 dpi 取整，容许 1 px 的取整差；90° 交换会差出几百 px，
    // 这点容差挡不住它。
    const SLACK: i64 = 1;

    let mut problems = Vec::new();
    for (i, ((frame, _expected), page)) in frames.iter().zip(expected_pages).zip(raster).enumerate()
    {
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

    let expected_draw = ordered_mutool_texts(manifest);

    // poppler 只被要求看到**同一组**块，不被要求同意它们的先后——那是
    // reading-order 检查的事，且并非每份 fixture 都适合断言它。
    let mut expected_set = ordered_texts(manifest, |block| block.draw_order);
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
    if !manifest.expected.block.is_empty()
        && manifest
            .expected
            .block
            .iter()
            .all(|block| !block.unicode_semantic)
    {
        let expected = manifest
            .expected
            .block
            .iter()
            .map(|block| text::compare_key(&block.text).chars().count())
            .sum::<usize>();
        return [
            ("glyphs/mutool", mutool_pages),
            ("glyphs/poppler", poppler_pages),
        ]
        .into_iter()
        .map(|(check, pages)| {
            let actual = flatten(pages)
                .iter()
                .map(|value| text::compare_key(value).chars().count())
                .sum::<usize>();
            if actual == expected {
                Outcome::ok(
                    check,
                    "§2.1/ADR-0015",
                    format!("Unicode 无语义；解析器独立报告 {actual} 个 glyph"),
                )
            } else {
                Outcome::fail(
                    check,
                    "§2.1/ADR-0015",
                    format!("Unicode 无语义；手写 {expected} 个 glyph，解析器给出 {actual} 个"),
                )
            }
        })
        .collect();
    }
    let expected_poppler = glyph_counts(
        &manifest
            .expected
            .block
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>(),
    );
    let expected_mutool = glyph_counts(&ordered_mutool_texts(manifest));
    [
        ("glyphs/mutool", mutool_pages, &expected_mutool),
        ("glyphs/poppler", poppler_pages, &expected_poppler),
    ]
    .into_iter()
    .map(|(check, pages, expected)| {
        let actual = glyph_counts(&flatten(pages));
        let mut problems = Vec::new();
        for (ch, n) in expected {
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

fn filter_nonlocatable_xobject_text(manifest: &Manifest, pages: &[ParsedPage]) -> Vec<ParsedPage> {
    if !manifest
        .identity
        .cases
        .iter()
        .any(|case_id| case_id == "XOBJ-08")
    {
        return pages.to_vec();
    }
    let declared: BTreeSet<String> = manifest
        .expected
        .block
        .iter()
        .map(|block| text::compare_key(&block.text))
        .collect();
    pages
        .iter()
        .cloned()
        .map(|mut page| {
            page.blocks
                .retain(|block| declared.contains(&text::compare_key(&block.text)));
            page
        })
        .collect()
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

fn ordered_mutool_texts(manifest: &Manifest) -> Vec<String> {
    let mut blocks: Vec<&crate::manifest::Block> = manifest
        .expected
        .block
        .iter()
        .filter(|block| block.mutool_extractable)
        .collect();
    blocks.sort_by_key(|block| block.draw_order);
    blocks
        .iter()
        .map(|block| text::compare_key(&block.text))
        .collect()
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

// ---------------------------------------------------------------- §2.2 手写三盒

fn check_hand_written_geometry(
    manifest: &Manifest,
    pdf: &Path,
    frames: &[PageFrame],
    mutool_pages: &[ParsedPage],
    poppler_pages: &[ParsedPage],
) -> Result<Vec<Outcome>> {
    let outlines = outline_blocks(manifest, pdf, frames)?;
    let mut baseline_problems = Vec::new();
    let mut metric_problems = Vec::new();
    let mut visual_problems = Vec::new();
    let arithmetic_tolerance = manifest.expected.tolerance_pt;
    let visual_tolerance = manifest
        .expected
        .visual_tolerance_pt
        .context("hand-written geometry missing visual_tolerance_pt")?;
    for block in &manifest.expected.block {
        let key = text::compare_key(&block.text);
        let draw_occurrence = manifest
            .expected
            .block
            .iter()
            .filter(|other| {
                other.page == block.page
                    && other.draw_order < block.draw_order
                    && other.mutool_extractable
                    && text::compare_key(&other.text) == key
            })
            .count();
        let reading_occurrence = manifest
            .expected
            .block
            .iter()
            .filter(|other| {
                other.page == block.page
                    && other.reading_order < block.reading_order
                    && text::compare_key(&other.text) == key
            })
            .count();
        let expected_baseline = block
            .baseline_origin
            .context("hand-written block missing baseline_origin")?;
        let expected_metric = block
            .metric_box
            .context("hand-written block missing metric_box")?;
        let expected_visual = block
            .visual_bbox
            .context("hand-written block missing visual_bbox")?;

        if block.mutool_extractable {
            let observed = if block.unicode_semantic {
                pick_on_page(mutool_pages, block.page, &key, draw_occurrence)
            } else {
                let ordinal = manifest
                    .expected
                    .block
                    .iter()
                    .filter(|other| {
                        other.page == block.page
                            && other.draw_order < block.draw_order
                            && other.mutool_extractable
                    })
                    .count();
                pick_on_page_by_ordinal(mutool_pages, block.page, ordinal)
            };
            match observed {
                Ok(observed) => match observed.baseline_origin {
                    Some(point)
                        if close(point.x, expected_baseline[0], arithmetic_tolerance)
                            && close(point.y, expected_baseline[1], arithmetic_tolerance) => {}
                    Some(point) => baseline_problems.push(format!(
                        "块 `{}`：manifest {:?}，mutool {:?}",
                        block.key,
                        expected_baseline,
                        point.to_array()
                    )),
                    None => baseline_problems
                        .push(format!("块 `{}`：mutool 未报告 baseline", block.key)),
                },
                Err(error) => baseline_problems.push(format!("块 `{}`：{error}", block.key)),
            }
        }

        let observed = if block.unicode_semantic {
            pick_on_page(poppler_pages, block.page, &key, reading_occurrence)
        } else {
            let ordinal = manifest
                .expected
                .block
                .iter()
                .filter(|other| {
                    other.page == block.page && other.reading_order < block.reading_order
                })
                .count();
            pick_on_page_by_ordinal(poppler_pages, block.page, ordinal)
        };
        match observed {
            Ok(observed)
                if arrays_close(
                    &observed.rect.to_array(),
                    &expected_metric,
                    arithmetic_tolerance,
                ) => {}
            Ok(observed) => metric_problems.push(format!(
                "块 `{}`：manifest {:?}，poppler {:?}",
                block.key,
                expected_metric,
                observed.rect.to_array()
            )),
            Err(error) => metric_problems.push(format!("块 `{}`：{error}", block.key)),
        }

        if block.visible && block.mutool_extractable {
            let observed_visual = outlines
                .get(&block.key)
                .with_context(|| format!("outline oracle missing block `{}`", block.key))?;
            if !arrays_close(
                &observed_visual.to_array(),
                &expected_visual,
                visual_tolerance,
            ) {
                visual_problems.push(format!(
                    "块 `{}`：manifest {:?}，mutool SVG {:?}",
                    block.key,
                    expected_visual,
                    observed_visual.to_array()
                ));
            }
        }
    }

    Ok(vec![
        geometry_outcome(
            "geometry/baseline",
            arithmetic_tolerance,
            baseline_problems,
            "mutool stext baseline",
        ),
        geometry_outcome(
            "geometry/metric-box",
            arithmetic_tolerance,
            metric_problems,
            "poppler bbox-layout",
        ),
        geometry_outcome(
            "geometry/visual-bbox",
            visual_tolerance,
            visual_problems,
            "mutool SVG glyph outlines",
        ),
    ])
}

fn check_transformed_text_geometry(
    manifest: &Manifest,
    pdf: &Path,
    frames: &[PageFrame],
) -> Result<Vec<Outcome>> {
    let qpdf_document = qpdf::Document::load(pdf)?;
    let font = manifest
        .source
        .fonts
        .first()
        .context("transformed-text-geometry requires one pinned font")?;
    let descriptor = font
        .descriptor_object
        .context("transformed-text-geometry font has no descriptor object")?;
    let ascent = qpdf_document.number(descriptor, "/Ascent")? / 1000.0;
    let descent = qpdf_document.number(descriptor, "/Descent")? / 1000.0;
    let expected_font = format!(
        "{}+{}",
        font.subset_tag
            .as_deref()
            .context("transformed-text-geometry font has no subset tag")?,
        font.base_name
            .as_deref()
            .context("transformed-text-geometry font has no base name")?
    );

    let mut blocks = manifest.expected.block.iter().collect::<Vec<_>>();
    blocks.sort_by_key(|block| block.draw_order);
    // A PDF space participates in text advance but has no outline block. Its
    // byte-level presence is covered by pdf-bytes; transformed geometry is
    // intentionally an ink-glyph oracle.
    let glyphs = mupdf_trace::glyphs(pdf)?
        .into_iter()
        .filter(|glyph| !glyph.unicode.chars().all(char::is_whitespace))
        .collect::<Vec<_>>();
    let outlines = outline_blocks(manifest, pdf, frames)?;
    let arithmetic_tolerance = manifest.expected.tolerance_pt;
    let visual_tolerance = manifest
        .expected
        .visual_tolerance_pt
        .context("transformed text geometry missing visual_tolerance_pt")?;
    let mut baseline_problems = Vec::new();
    let mut metric_problems = Vec::new();
    let mut visual_problems = Vec::new();

    if blocks.len() != glyphs.len() {
        let problem = format!(
            "manifest has {} transformed blocks but MuPDF trace has {} glyphs",
            blocks.len(),
            glyphs.len()
        );
        baseline_problems.push(problem.clone());
        metric_problems.push(problem);
    }
    for (block, glyph) in blocks.into_iter().zip(&glyphs) {
        if block.text.chars().count() != 1 || glyph.unicode != block.text {
            let problem = format!(
                "block {} expected one glyph {:?}, trace reported {:?}",
                block.key, block.text, glyph.unicode
            );
            baseline_problems.push(problem.clone());
            metric_problems.push(problem);
            continue;
        }
        if glyph.font != expected_font {
            metric_problems.push(format!(
                "block {} trace font {:?}, expected {:?}",
                block.key, glyph.font, expected_font
            ));
        }
        let expected_baseline = block
            .baseline_origin
            .context("transformed block missing baseline_origin")?;
        if !close(glyph.origin[0], expected_baseline[0], arithmetic_tolerance)
            || !close(glyph.origin[1], expected_baseline[1], arithmetic_tolerance)
        {
            baseline_problems.push(format!(
                "block {} manifest {:?}, trace {:?}",
                block.key, expected_baseline, glyph.origin
            ));
        }
        let expected_metric = block
            .metric_box
            .context("transformed block missing metric_box")?;
        let actual_metric = glyph.metric_box(ascent, descent).to_array();
        if !arrays_close(&actual_metric, &expected_metric, arithmetic_tolerance) {
            metric_problems.push(format!(
                "block {} manifest {:?}, qpdf+trace {:?}",
                block.key, expected_metric, actual_metric
            ));
        }
        let expected_visual = block
            .visual_bbox
            .context("transformed block missing visual_bbox")?;
        let actual_visual = outlines
            .get(&block.key)
            .with_context(|| format!("outline oracle missing block `{}`", block.key))?
            .to_array();
        if !arrays_close(&actual_visual, &expected_visual, visual_tolerance) {
            visual_problems.push(format!(
                "block {} manifest {:?}, MuPDF SVG {:?}",
                block.key, expected_visual, actual_visual
            ));
        }
    }

    Ok(vec![
        geometry_outcome(
            "geometry/transform-baseline",
            arithmetic_tolerance,
            baseline_problems,
            "MuPDF trace glyph origin",
        ),
        geometry_outcome(
            "geometry/transform-metric",
            arithmetic_tolerance,
            metric_problems,
            "qpdf descriptor metrics composed with MuPDF trace trm/advance",
        ),
        geometry_outcome(
            "geometry/transform-visual",
            visual_tolerance,
            visual_problems,
            "MuPDF SVG glyph outlines",
        ),
    ])
}

fn check_type3_geometry(manifest: &Manifest, mutool_pages: &[ParsedPage]) -> Vec<Outcome> {
    let observed = flatten_blocks(mutool_pages);
    let tolerance = manifest.expected.tolerance_pt;
    let mut baseline_problems = Vec::new();
    let mut bbox_problems = Vec::new();
    for block in &manifest.expected.block {
        let key = text::compare_key(&block.text);
        let Ok(actual) = pick(&observed, &key) else {
            baseline_problems.push(format!("block {} missing from mutool trace", block.key));
            continue;
        };
        let expected_baseline = block.baseline_origin.expect("validated baseline");
        let expected_bbox = block.visual_bbox.expect("validated visual bbox");
        let Some(actual_baseline) = actual.baseline_origin else {
            baseline_problems.push(format!("block {} has no mutool baseline", block.key));
            continue;
        };
        if !close(actual_baseline.x, expected_baseline[0], tolerance)
            || !close(actual_baseline.y, expected_baseline[1], tolerance)
        {
            baseline_problems.push(format!(
                "{} expected {:?}, mutool {:?}",
                block.key, expected_baseline, actual_baseline
            ));
        }
        if !arrays_close(&actual.rect.to_array(), &expected_bbox, tolerance) {
            bbox_problems.push(format!(
                "{} expected {:?}, mutool {:?}",
                block.key,
                expected_bbox,
                actual.rect.to_array()
            ));
        }
    }
    vec![
        geometry_outcome(
            "geometry/type3-baseline",
            tolerance,
            baseline_problems,
            "mutool Type3 trace baseline",
        ),
        geometry_outcome(
            "geometry/type3-painted-bbox",
            tolerance,
            bbox_problems,
            "mutool painted CharProc bbox",
        ),
    ]
}

fn check_font_advance(manifest: &Manifest, mutool_pages: &[ParsedPage]) -> Vec<Outcome> {
    let observed = flatten_blocks(mutool_pages);
    let tolerance = manifest.expected.tolerance_pt;
    let mut baseline_problems = Vec::new();
    let mut advance_problems = Vec::new();
    for block in &manifest.expected.block {
        let key = text::compare_key(&block.text);
        let Ok(actual) = pick(&observed, &key) else {
            baseline_problems.push(format!("block {} missing from mutool trace", block.key));
            continue;
        };
        let expected_baseline = block.baseline_origin.expect("validated baseline");
        let expected_metric = block.metric_box.expect("validated metric box");
        let Some(actual_baseline) = actual.baseline_origin else {
            baseline_problems.push(format!("block {} has no mutool baseline", block.key));
            continue;
        };
        if !close(actual_baseline.x, expected_baseline[0], tolerance)
            || !close(actual_baseline.y, expected_baseline[1], tolerance)
        {
            baseline_problems.push(format!(
                "{} expected {:?}, mutool {:?}",
                block.key, expected_baseline, actual_baseline
            ));
        }
        if !close(actual.rect.x0, expected_metric[0], tolerance)
            || !close(actual.rect.x1, expected_metric[2], tolerance)
        {
            advance_problems.push(format!(
                "{} expected x [{},{}], mutool [{},{}]",
                block.key, expected_metric[0], expected_metric[2], actual.rect.x0, actual.rect.x1
            ));
        }
    }
    vec![
        geometry_outcome(
            "geometry/font-baseline",
            tolerance,
            baseline_problems,
            "mutool text baseline",
        ),
        geometry_outcome(
            "geometry/font-advance",
            tolerance,
            advance_problems,
            "mutool horizontal advance",
        ),
    ]
}

fn check_embedded_cmap(manifest: &Manifest, mutool_pages: &[ParsedPage]) -> Result<Vec<Outcome>> {
    let font = manifest
        .source
        .fonts
        .first()
        .context("embedded-cmap check missing pinned font")?;
    let font_bytes = std::fs::read(manifest.dir.join(&font.file))?;
    let face = ttf_parser::Face::parse(&font_bytes, 0).context("parse pinned TrueType font")?;
    let expected_text = manifest
        .expected
        .block
        .first()
        .context("embedded-cmap check missing expected block")?;
    let mapped: Vec<u16> = expected_text
        .text
        .chars()
        .map(|character| {
            face.glyph_index(character)
                .map(|glyph| glyph.0)
                .with_context(|| format!("pinned font has no glyph for {character:?}"))
        })
        .collect::<Result<_>>()?;
    let cmap_outcome = if mapped == manifest.expected.cid_sequence {
        Outcome::ok(
            "embedded-cmap",
            "§2.8/CMAP-04",
            format!("pinned TTF maps {:?} to {:?}", expected_text.text, mapped),
        )
    } else {
        Outcome::fail(
            "embedded-cmap",
            "§2.8/CMAP-04",
            format!(
                "manifest CID sequence {:?}, pinned TTF glyph IDs {:?}",
                manifest.expected.cid_sequence, mapped
            ),
        )
    };

    let observed = flatten_blocks(mutool_pages);
    let mut geometry_problems = Vec::new();
    let identity_alias =
        manifest.identity.cases.iter().any(|case| case == "CMAP-02") && observed.is_empty();
    if identity_alias {
        // MuPDF and Poppler do not implement the legal Distiller DLIdent-H/V
        // aliases. The static half of this oracle still proves the raw CID
        // sequence against the pinned font cmap; production-path tests own
        // alias recognition and paragraph behavior.
    } else if observed.len() != 1 {
        geometry_problems.push(format!(
            "mutool reported {} blocks, expected 1",
            observed.len()
        ));
    } else {
        let actual = &observed[0];
        let expected_baseline = expected_text.baseline_origin.expect("validated baseline");
        let expected_metric = expected_text.metric_box.expect("validated metric box");
        let expected_visual = expected_text.visual_bbox.expect("validated visual bbox");
        if actual.text.chars().count() != manifest.expected.cid_sequence.len() {
            geometry_problems.push(format!(
                "mutool reported {} glyphs, expected {}",
                actual.text.chars().count(),
                manifest.expected.cid_sequence.len()
            ));
        }
        if actual.baseline_origin.is_none_or(|point| {
            !close(
                point.x,
                expected_baseline[0],
                manifest.expected.tolerance_pt,
            ) || !close(
                point.y,
                expected_baseline[1],
                manifest.expected.tolerance_pt,
            )
        }) || !close(
            actual.rect.x0,
            expected_metric[0],
            manifest.expected.tolerance_pt,
        ) || !close(
            actual.rect.x1,
            expected_metric[2],
            manifest.expected.tolerance_pt,
        ) || !close(
            actual.rect.y0,
            expected_visual[1],
            manifest.expected.visual_tolerance_pt.unwrap_or(0.01),
        ) || !close(
            actual.rect.y1,
            expected_visual[3],
            manifest.expected.visual_tolerance_pt.unwrap_or(0.01),
        ) {
            geometry_problems.push(format!(
                "expected baseline {:?}/metric-x [{},{}]/ink-y [{},{}], mutool {:?}/{:?}",
                expected_baseline,
                expected_metric[0],
                expected_metric[2],
                expected_visual[1],
                expected_visual[3],
                actual.baseline_origin,
                actual.rect.to_array()
            ));
        }
    }
    let geometry_outcome = if identity_alias {
        Outcome::ok(
            "geometry/cid-glyphs",
            "§2.8/CMAP-02",
            "MuPDF/Poppler expose no glyphs for DLIdent-H; static CID/cmap proof passed",
        )
    } else {
        geometry_outcome(
            "geometry/cid-glyphs",
            manifest.expected.visual_tolerance_pt.unwrap_or(0.01),
            geometry_problems,
            "mutool glyph count/baseline/ink bbox",
        )
    };
    Ok(vec![cmap_outcome, geometry_outcome])
}

fn geometry_outcome(
    check: &'static str,
    tolerance: f64,
    problems: Vec<String>,
    source: &str,
) -> Outcome {
    if problems.is_empty() {
        Outcome::ok(
            check,
            "§2.2",
            format!("与手写值一致（{source}，容差 {tolerance} pt）"),
        )
    } else {
        Outcome::fail(check, "§2.2", problems.join("；"))
    }
}

fn outline_blocks(
    manifest: &Manifest,
    pdf: &Path,
    frames: &[PageFrame],
) -> Result<BTreeMap<String, Rect>> {
    let mut by_page: BTreeMap<usize, Vec<&crate::manifest::Block>> = BTreeMap::new();
    for block in manifest
        .expected
        .block
        .iter()
        .filter(|block| block.visible && block.mutool_extractable)
    {
        by_page.entry(block.page).or_default().push(block);
    }
    for blocks in by_page.values_mut() {
        blocks.sort_by_key(|block| block.draw_order);
    }

    let mut result = BTreeMap::new();
    for (page, blocks) in by_page {
        let glyphs = mupdf_svg::glyphs(pdf, page)?;
        let frame = frames
            .get(page)
            .with_context(|| format!("outline page {page} has no PageFrame"))?;
        let mut cursor = 0usize;
        for block in blocks {
            let expected: Vec<char> = block
                .visual_text
                .as_deref()
                .unwrap_or(&block.text)
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            let mut bounds: Option<Rect> = None;
            for character in expected {
                let glyph = glyphs.get(cursor).with_context(|| {
                    format!("块 `{}` 的 SVG glyph 在第 {cursor} 个处提前结束", block.key)
                })?;
                let mut observed = glyph.text.chars();
                let scalar = observed.next().context("SVG glyph data-text is empty")?;
                if block.unicode_semantic && (observed.next().is_some() || scalar != character) {
                    bail!(
                        "块 `{}` 第 {} 个 glyph：期望 {character:?}，mutool SVG 给出 {:?}",
                        block.key,
                        cursor + 1,
                        glyph.text
                    );
                }
                let rect =
                    frame.rect_to_page(glyph.rect.x0, glyph.rect.y0, glyph.rect.x1, glyph.rect.y1);
                bounds = Some(match bounds {
                    Some(current) => current.union(rect),
                    None => rect,
                });
                cursor += 1;
            }
            result.insert(
                block.key.clone(),
                bounds.with_context(|| format!("块 `{}` 没有可见 glyph", block.key))?,
            );
        }
        if cursor != glyphs.len() {
            bail!(
                "第 {page} 页有 {} 个未被手写 block 消费的 SVG glyph",
                glyphs.len() - cursor
            );
        }
    }
    Ok(result)
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

fn pick_on_page<'a>(
    pages: &'a [ParsedPage],
    page_index: usize,
    key: &str,
    occurrence: usize,
) -> Result<&'a crate::oracle::ParsedBlock, String> {
    let page = pages
        .iter()
        .find(|page| page.index == page_index)
        .ok_or_else(|| format!("找不到第 {} 页的解析器输出", page_index + 1))?;
    page.blocks
        .iter()
        .filter(|block| text::compare_key(&block.text) == key)
        .nth(occurrence)
        .ok_or_else(|| {
            format!(
                "第 {} 页找不到文本 {key:?} 的第 {} 次出现",
                page_index + 1,
                occurrence + 1
            )
        })
}

fn pick_on_page_by_ordinal(
    pages: &[ParsedPage],
    page_index: usize,
    ordinal: usize,
) -> Result<&crate::oracle::ParsedBlock, String> {
    let page = pages
        .iter()
        .find(|page| page.index == page_index)
        .ok_or_else(|| format!("找不到第 {} 页的解析器输出", page_index + 1))?;
    page.blocks.get(ordinal).ok_or_else(|| {
        format!(
            "第 {} 页找不到绘制序号为 {} 的块",
            page_index + 1,
            ordinal + 1
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
    let all_manifests = discover(repo_root)?;
    for manifest in manifests {
        let target = manifest.pdf_path();
        let sha = match manifest.source.method {
            Method::RealisticTypesetting => {
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
                if built != target {
                    std::fs::rename(&built, &target).with_context(|| {
                        format!("把 {} 挪到 {} 失败", built.display(), target.display())
                    })?;
                }
                hash::of_file(&target)?
            }
            Method::ExactWriter => exact::write_atomic(manifest.id(), repo_root, &target)?,
            Method::ByteMutation => {
                let lineage = manifest
                    .lineage
                    .as_ref()
                    .context("byte-mutation fixture 缺少 lineage")?;
                let parent = all_manifests
                    .iter()
                    .find(|candidate| candidate.id() == lineage.parent)
                    .with_context(|| format!("找不到合法父本 `{}`", lineage.parent))?;
                let mutation = lineage
                    .mutations
                    .first()
                    .context("byte-mutation fixture 没有变异记录")?;
                let parent_bytes = std::fs::read(parent.pdf_path())?;
                let derived = mutation::derive(
                    &parent_bytes,
                    MutationSpec {
                        parent_fixture_id: &lineage.parent,
                        byte_offset: usize::try_from(mutation.byte_offset)
                            .context("mutation offset exceeds usize")?,
                        expected_bytes: &mutation.original_bytes,
                        replacement_bytes: &mutation.replacement_bytes,
                        semantics: &mutation.description,
                    },
                )?;
                exact::write_bytes_atomic(&derived.bytes, &target)?
            }
            Method::ToolGeneratedCommitted => {
                if !target.is_file() {
                    bail!(
                        "[{}] tool-generated-committed PDF 必须先由声明的 argv 一次性生成并入库",
                        manifest.id()
                    );
                }
                hash::of_file(&target)?
            }
        };

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

    fn outline_document(last: u32) -> qpdf::Document {
        qpdf::Document::parse(&format!(
            r#"{{
              "qpdf": [
                {{"jsonversion": 2, "pdfversion": "1.7", "maxobjectid": 11}},
                {{
                  "obj:8 0 R": {{"value": {{"/First": "9 0 R", "/Last": "{last} 0 R"}}}},
                  "obj:9 0 R": {{"value": {{"/Parent": "8 0 R", "/First": "11 0 R", "/Last": "11 0 R", "/Next": "10 0 R"}}}},
                  "obj:10 0 R": {{"value": {{"/Parent": "8 0 R", "/Prev": "9 0 R"}}}},
                  "obj:11 0 R": {{"value": {{"/Parent": "9 0 R"}}}}
                }}
              ]
            }}"#
        ))
        .unwrap()
    }

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
    fn walks_the_complete_outline_hierarchy_and_checks_sibling_links() {
        let observed = walk_outline(&outline_document(10), 8).unwrap();
        assert_eq!(
            observed,
            vec![
                ObservedBookmark {
                    object: 9,
                    parent: 8,
                    level: 1,
                },
                ObservedBookmark {
                    object: 11,
                    parent: 9,
                    level: 2,
                },
                ObservedBookmark {
                    object: 10,
                    parent: 8,
                    level: 1,
                },
            ]
        );

        let error = walk_outline(&outline_document(9), 8)
            .unwrap_err()
            .to_string();
        assert!(error.contains("/Last"), "{error}");
    }

    #[test]
    fn exact_structure_sets_reject_undeclared_extra_objects() {
        let mut problems = Vec::new();
        compare_object_set("annotations", &[12], vec![12, 13], &mut problems);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("13"), "{:?}", problems);
    }

    #[test]
    fn object_order_uses_xref_offsets_not_stream_markers() {
        let bytes = b"stream\n1 0 obj\nendstream\n2 0 obj\n(two)\nendobj\n1 0 obj\n(one)\nendobj\n";
        let real_object_2 = find_bytes(bytes, b"2 0 obj\n").unwrap();
        let real_object_1 = bytes
            .windows(b"1 0 obj\n".len())
            .rposition(|window| window == b"1 0 obj\n")
            .unwrap();
        let xref_offsets = BTreeMap::from([(1, real_object_1), (2, real_object_2)]);

        let problems = object_plan_problems(bytes, &[1, 2], &xref_offsets, true);

        assert!(
            problems.iter().any(|problem| problem.contains("写出顺序")),
            "{problems:?}"
        );
    }

    #[test]
    fn xref_offset_must_point_to_the_exact_object_header() {
        let bytes = b"garbage before 1 0 obj\n(one)\nendobj\n";
        let xref_offsets = BTreeMap::from([(1, 0)]);

        let problems = object_plan_problems(bytes, &[1], &xref_offsets, true);

        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("不指向精确对象头")),
            "{problems:?}"
        );
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
