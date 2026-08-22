//! expected manifest 的 schema 与校验（docs/03-corpus-requirements.md §2.7）。
//!
//! Manifest 是规格，PDF 是实现（§2.1）。因此这里的校验刻意严格：缺字段、字段
//! 之间自相矛盾、或者给现实排版之外的 fixture 用了 §2.1 的裁定例外，都在读取
//! 阶段就失败——manifest 一旦被生成结果反向影响，整份语料的价值就没了。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{exact, hash};

pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// 合同条款编号，用于把失败信息定位到具体条款。
pub type Clause = &'static str;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub identity: Identity,
    pub source: Source,
    /// 谱系：畸形 fixture 必填（§2.7）。
    #[serde(default)]
    pub lineage: Option<Lineage>,
    #[serde(default)]
    pub page: Vec<Page>,
    pub expected: Expected,
    pub oracle: Oracle,
    /// 跨 fixture 断言（如「三份顺序变体渲染逐像素一致」）。
    #[serde(default)]
    pub group: Option<Group>,
    /// §2.1 要求的裁定记录：manifest 与生成器每发生一次不一致就留一行。
    #[serde(default)]
    pub adjudication: Vec<Adjudication>,

    /// manifest 自身所在目录，读取时填入，不来自 TOML。
    #[serde(skip)]
    pub dir: PathBuf,
}

// ---------------------------------------------------------------- 身份

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    Unit,
    Integration,
    Mal,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Legality {
    Legal,
    Malformed,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
pub enum Priority {
    M0,
    M1,
    M3,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub class: Class,
    pub legality: Legality,
    /// 覆盖的需求矩阵 case ID。
    pub cases: Vec<String>,
    pub priority: Priority,
    /// §2.4 单变量原则：这份 fixture 引入的那**一个**变量。
    pub variable: String,
}

// ---------------------------------------------------------------- 来源

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Method {
    /// 手写 content stream + 自研确定性 writer。
    ExactWriter,
    /// 钉死版本的 Typst / LaTeX。
    RealisticTypesetting,
    /// 从合法父本做字节级变异。
    ByteMutation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub method: Method,
    /// Corpus-owned generator contract. Exact fixtures pin this to the
    /// versioned raw-byte writer; realistic fixtures pin an external engine.
    #[serde(default)]
    pub generator: Option<String>,
    /// `corpus/toolchain.toml` 里的 `[[engine]].id`；现实排版 fixture 必填。
    #[serde(default)]
    pub engine: Option<String>,
    /// 生成源文件，相对 manifest 所在目录。
    #[serde(default)]
    pub source_file: Option<String>,
    /// 产物 PDF，相对 manifest 所在目录。
    pub pdf: String,
    pub pdf_sha256: String,
    /// 字体来源。现实排版 fixture 由钉死的工具版本担保（§2.6）。
    pub font_provenance: FontProvenance,
    /// 精确 fixture 必填：钉死的字体文件及其 SHA-256。
    #[serde(default)]
    pub fonts: Vec<FontPin>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum FontProvenance {
    /// Typst 二进制内嵌字体集合，由钉死的 Typst 版本担保。
    TypstEmbedded,
    /// TeX Live 2026 自带字体，由钉死的发行版担保。
    TexliveBundled,
    /// 仓库内 vendored 的字体文件，必须逐个记 SHA-256。
    Vendored,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontPin {
    pub file: String,
    pub sha256: String,
    /// Exact-fixture traceability: indirect font and descriptor objects plus
    /// the embedded PDF font name. Realistic fixtures leave these unset.
    #[serde(default)]
    pub pdf_object: Option<u32>,
    #[serde(default)]
    pub descriptor_object: Option<u32>,
    #[serde(default)]
    pub subset_tag: Option<String>,
    #[serde(default)]
    pub base_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lineage {
    /// 合法父本的 fixture ID。
    pub parent: String,
    pub mutations: Vec<Mutation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutation {
    pub byte_offset: u64,
    pub original_byte: u8,
    pub replacement_byte: u8,
    pub description: String,
}

// ---------------------------------------------------------------- 页面几何

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub index: usize,
    /// §2.2：页面空间由 MediaBox 定义，原点不必是 (0,0)。
    pub media_box: [f64; 4],
    /// 缺失时等同 MediaBox（§2.3）。
    #[serde(default)]
    pub crop_box: Option<[f64; 4]>,
    pub rotate: i32,
}

impl Expected {
    /// 墨迹盒越出度量盒的允许余量。
    pub fn ink_margin(&self) -> f64 {
        self.ink_margin_pt.unwrap_or(self.tolerance_pt)
    }
}

impl Page {
    /// 观看用的有效框：CropBox 优先，缺失时退回 MediaBox。
    pub fn effective_box(&self) -> [f64; 4] {
        self.crop_box.unwrap_or(self.media_box)
    }
}

// ---------------------------------------------------------------- 期望

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum GeometrySource {
    /// 由 PDF 规范与字体度量推导，先于生成器手写（§2.1 第一原则）。
    HandWritten,
    /// 由 poppler 与 mutool 两个独立解析器一致确立（§2.1 唯一例外）。
    DualParserAdjudicated,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub geometry_source: GeometrySource,
    /// 几何一致性容差。现实排版 fixture 只能取到两个解析器的一致精度（§2.1）。
    ///
    /// 只用于**两个解析器报告同一个量**的场合：页面框尺寸、块的 x 跨度。
    pub tolerance_pt: f64,
    /// Exact outline bbox tolerance (§2.2); distinct from arithmetic geometry.
    #[serde(default)]
    pub visual_tolerance_pt: Option<f64>,
    /// 墨迹盒允许越出度量盒的余量；省略时取 `tolerance_pt`。
    ///
    /// 这是另一码事：墨迹盒（mutool 的字形 quad）与度量盒（poppler 的词盒）是
    /// **两个不同的量**，它们之间只有包含关系，而余量的大小是字体度量的性质，
    /// 不是解析器分歧。实测 Typst 内嵌字体下余量 < 0.05pt，而 TeX Live 的
    /// Computer Modern Type1 下墨迹会越出约 0.22pt。用同一个数去卡两件事，
    /// 要么放松了 x 跨度的判据，要么把正常的字体度差判成解析器不一致。
    #[serde(default)]
    pub ink_margin_pt: Option<f64>,
    pub structure: Structure,
    pub block: Vec<Block>,
    /// Exact byte/object contract. Required for exact-writer fixtures.
    #[serde(default)]
    pub pdf: Option<PdfContract>,
    #[serde(default)]
    pub content_stream: Vec<ContentStream>,
    #[serde(default)]
    pub reference: Vec<ObjectReference>,
    #[serde(default)]
    pub bookmark: Vec<Bookmark>,
    #[serde(default)]
    pub annotation: Vec<Annotation>,
    #[serde(default)]
    pub named_destination: Vec<NamedDestination>,
    #[serde(default)]
    pub optional_content_group: Vec<OptionalContentGroup>,
    pub behaviour: Vec<Behaviour>,
    /// 畸形 fixture 必填。结构畸形写 qpdf 输出中必须出现的子串；content
    /// 语义畸形写 `operator-walk:<stable-error-id>`，此时 qpdf 必须确认 PDF
    /// 容器本身合法，错误由实验 2 的隔离走查器断言。
    #[serde(default)]
    pub declared_failure: Option<String>,
    /// Independent renderer diagnostic required for a malformed fixture that
    /// cannot produce a reference raster by design.
    #[serde(default)]
    pub renderer_diagnostic: Option<String>,
}

/// 结构化期望——**先于生成写死**，生成结果不符即为失败（§2.1 例外条款）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Structure {
    pub pages: usize,
    pub blocks: usize,
    /// 栏数；单栏写 1。
    pub columns: usize,
    #[serde(default)]
    pub bookmarks: usize,
    #[serde(default)]
    pub bookmark_depth: usize,
    #[serde(default)]
    pub annotations: usize,
    #[serde(default)]
    pub form_fields: usize,
    #[serde(default)]
    pub optional_content_groups: usize,
    #[serde(default)]
    pub named_destinations: usize,
    #[serde(default)]
    pub uri_actions: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfContract {
    pub version: String,
    pub header_prefix_hex: String,
    pub object_numbers: Vec<u32>,
    pub root_object: u32,
    pub xref_kind: XrefKind,
    pub trailer_id_hex: String,
    pub info_dictionary: bool,
    pub metadata_streams: usize,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum XrefKind {
    Table,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentStream {
    pub object: u32,
    pub compressed: bool,
    #[serde(default)]
    pub bytes: String,
    #[serde(default)]
    pub bytes_hex: Option<String>,
    #[serde(default)]
    pub decoded_bytes: Option<String>,
    #[serde(default)]
    pub decoded_bytes_hex: Option<String>,
    #[serde(default)]
    pub filters: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectReference {
    pub from_object: u32,
    pub path: Vec<String>,
    pub to_object: u32,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum BookmarkTarget {
    Xyz,
    Named,
    Uri,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    pub object: u32,
    pub parent_object: u32,
    pub level: usize,
    pub title: String,
    #[serde(default)]
    pub count: Option<i32>,
    #[serde(default)]
    pub color: Option<[f64; 3]>,
    pub style_flags: u32,
    pub target: BookmarkTarget,
    #[serde(default)]
    pub page_object: Option<u32>,
    #[serde(default)]
    pub xyz: Option<[f64; 3]>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationSubtype {
    Link,
    Text,
    Widget,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub object: u32,
    pub subtype: AnnotationSubtype,
    pub rect: [f64; 4],
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub contents: Option<String>,
    #[serde(default)]
    pub field_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedDestination {
    pub name: String,
    pub page_object: u32,
    pub xyz: [f64; 3],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionalContentGroup {
    pub object: u32,
    pub name: String,
    pub resource_name: String,
    pub initially_visible: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    /// 人可读的稳定键，如 `L1` / `R2`。
    pub key: String,
    pub page: usize,
    /// 该块属于哪一栏（`left` / `right` / `full`）。
    pub column: String,
    /// 在 content stream 中的绘制次序，1 起。
    pub draw_order: usize,
    /// 人工确定的阅读次序，1 起。
    pub reading_order: usize,
    /// 该块的完整文本；比较前按 `text::normalize` 归一。
    pub text: String,
    /// 手写几何（`geometry_source = "hand-written"` 时必填）。
    #[serde(default)]
    pub baseline_origin: Option<[f64; 2]>,
    #[serde(default)]
    pub metric_box: Option<[f64; 4]>,
    #[serde(default)]
    pub visual_bbox: Option<[f64; 4]>,
}

/// §2.9：断言必须独立可观察。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Behaviour {
    pub id: String,
    pub assertion: String,
    /// 用什么手段观察这条断言。
    pub observable_via: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oracle {
    pub checks: Vec<Check>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Check {
    /// §2.8 步骤 1：重新生成，SHA-256 必须一致。
    Determinism,
    /// §2.8 步骤 2：合法性核验。
    Legality,
    /// §2.2 / §2.3：逐页 MediaBox / CropBox / Rotate。
    PageGeometry,
    /// §2.1 例外：结构化期望——块数、文本、以及 mutool 眼中的**绘制顺序**。
    Structure,
    /// §2.1 例外（弱化版）：两个解析器各自看到的**字符多重集**与手写文本相同。
    ///
    /// 给那些块划分或块内字形次序无法被两个解析器一致裁定的 fixture 用——
    /// 目录的右对齐页码在 poppler 那里自成一块而在 mutool 那里不是；上下标公式
    /// 的字形次序两者根本不同。这类 fixture 的次序不可断言，但「这一页上有且
    /// 只有这些字符」仍然可断言，而且足以挡住掉字、多字、字体映射错误。
    Glyphs,
    /// §2.1 例外：poppler 版面分析给出的**阅读顺序**与手写 reading_order 一致。
    ///
    /// 刻意与 `structure` 分开声明：poppler 的分栏判定有自己的栏间距阈值，
    /// 栏间距窄到一定程度它就退化成行优先。那种 fixture（LAYOUT-08）恰恰是
    /// 要把这件事本身当作被观察对象，不能让门禁替它断言一个「正确」的阅读序。
    ReadingOrder,
    /// §2.1 例外：poppler 与 mutool 的几何一致裁定。
    DualParserGeometry,
    /// §2.2: compare all three hand-written geometry quantities independently.
    HandWrittenGeometry,
    /// Type3 d1 geometry: MuPDF independently observes baseline and the
    /// painted CharProc bbox. Poppler's synthesized Type3 metric box is kept
    /// as differential evidence rather than treated as the specification.
    Type3Geometry,
    /// Exact header/object/xref/trailer/content-stream bytes.
    PdfBytes,
    /// Rich outline/action/annotation/form/OCG/name-tree object graph.
    PdfStructure,
    /// §2.8 步骤 4：独立渲染器出图并存参考栅格哈希。
    Render,
    /// A malformed fixture is independently observed through a stable mutool
    /// trace diagnostic instead of a successful reference raster.
    RenderDiagnostic,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub name: String,
    /// 与这些 fixture 的渲染必须逐像素一致。
    #[serde(default)]
    pub raster_identical_with: Vec<String>,
    /// 与这些 fixture 的 content stream 绘制顺序必须**不同**。
    #[serde(default)]
    pub draw_order_differs_from: Vec<String>,
    /// 与这些 fixture 的**页面空间**块几何必须逐块相同。
    ///
    /// 这是 GEOM-04 的判据：五个 `/Rotate` 取值下内容完全相同，意味着栅格必然
    /// 不同（朝向不同）而页面空间几何必须一致。渲染哈希在这里帮不上忙，能判的
    /// 只有换算回页面空间之后的盒子。
    #[serde(default)]
    pub page_geometry_identical_with: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adjudication {
    pub date: String,
    pub issue: String,
    pub resolution: String,
}

// ---------------------------------------------------------------- 读取与校验

impl Manifest {
    /// 从 fixture 目录读取并校验 manifest。
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("manifest.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 {} 失败", path.display()))?;
        let mut manifest: Self =
            toml::from_str(&text).with_context(|| format!("解析 {} 失败", path.display()))?;
        manifest.dir = dir.to_path_buf();
        manifest
            .validate()
            .with_context(|| format!("{} 不满足生成合同", path.display()))?;
        Ok(manifest)
    }

    pub fn id(&self) -> &str {
        &self.identity.id
    }

    pub fn pdf_path(&self) -> PathBuf {
        self.dir.join(&self.source.pdf)
    }

    pub fn adjudicated_path(&self) -> PathBuf {
        self.dir.join("adjudicated.toml")
    }

    /// 生成源文件的**仓库相对**路径，供引擎配方使用。
    pub fn source_path(&self, repo_root: &Path) -> Result<String> {
        let file = self
            .source
            .source_file
            .as_ref()
            .context("本 fixture 没有声明 source_file")?;
        let abs = self.dir.join(file);
        let rel = abs
            .strip_prefix(repo_root)
            .with_context(|| format!("{} 不在仓库内", abs.display()))?;
        Ok(rel.display().to_string())
    }

    pub fn requires(&self, check: Check) -> bool {
        self.oracle.checks.contains(&check)
    }

    fn validate(&self) -> Result<()> {
        fail_if(
            self.schema_version != SUPPORTED_SCHEMA_VERSION,
            "§2.7",
            &format!(
                "schema_version = {}，本工具只支持 {SUPPORTED_SCHEMA_VERSION}",
                self.schema_version
            ),
        )?;

        self.validate_identity()?;
        self.validate_source()?;
        self.validate_pages()?;
        self.validate_expected()?;

        fail_if(
            self.oracle.checks.is_empty(),
            "§2.8",
            "oracle.checks 为空——没有验收手段的 fixture 不得入库",
        )?;
        // 手写的 block.text 必须被至少一种手段核对。否则那一串文本就只是注释：
        // 写错了没人知道，而它恰恰是 §2.1 里最该被守住的东西。
        fail_if(
            !self.expected.block.is_empty()
                && !self.requires(Check::Structure)
                && !self.requires(Check::Glyphs),
            "§2.1",
            "手写的 block.text 无人核对——oracle.checks 必须含 structure 或 glyphs 之一",
        )?;

        Ok(())
    }

    fn validate_identity(&self) -> Result<()> {
        let dir_name = self
            .dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        fail_if(
            !dir_name.is_empty() && dir_name != self.identity.id,
            "§2.11",
            &format!(
                "目录名 `{dir_name}` 与 fixture ID `{}` 不一致",
                self.identity.id
            ),
        )?;

        let expected_prefix = match self.identity.class {
            Class::Unit => "unit-",
            Class::Integration => "intg-",
            Class::Mal => "mal-",
        };
        fail_if(
            !self.identity.id.starts_with(expected_prefix),
            "§2.11",
            &format!(
                "class = {:?} 要求 ID 以 `{expected_prefix}` 开头，实际是 `{}`",
                self.identity.class, self.identity.id
            ),
        )?;

        // 畸形与谱系必须同时成立：没有可追溯父本的「畸形」无法断言是哪处变异
        // 导致了行为（§2.5）。
        let malformed = self.identity.legality == Legality::Malformed;
        fail_if(
            (self.identity.class == Class::Mal) != malformed,
            "§2.11",
            "class = mal 与 legality = malformed 必须同时成立",
        )?;
        fail_if(
            malformed && self.lineage.is_none(),
            "§2.5",
            "畸形 fixture 必须声明 [lineage]：合法父本 + 变异位置",
        )?;
        fail_if(
            !malformed && self.lineage.is_some(),
            "§2.5",
            "合法 fixture 不应有 [lineage]",
        )?;
        if let Some(lineage) = &self.lineage {
            fail_if(
                self.source.method != Method::ByteMutation,
                "§2.5",
                "带 lineage 的 fixture 必须使用 method = \"byte-mutation\"",
            )?;
            fail_if(
                lineage.mutations.len() != 1,
                "§2.4/§2.5",
                "畸形派生必须且只能声明一处字节变异",
            )?;
            let mutation = &lineage.mutations[0];
            fail_if(
                mutation.original_byte == mutation.replacement_byte,
                "§2.4",
                "变异前后 byte 必须不同",
            )?;
            fail_if(
                mutation.description.trim().is_empty(),
                "§2.7",
                "变异语义描述不得为空",
            )?;
        }

        fail_if(
            self.identity.cases.is_empty(),
            "§2.7",
            "identity.cases 为空——fixture 必须声明它覆盖的 case",
        )?;
        fail_if(
            self.identity.variable.trim().is_empty(),
            "§2.4",
            "identity.variable 为空——单变量原则要求写明引入的那一个变量",
        )
    }

    fn validate_source(&self) -> Result<()> {
        match self.source.method {
            Method::RealisticTypesetting => {
                fail_if(
                    self.source.engine.is_none() || self.source.source_file.is_none(),
                    "§2.5",
                    "现实排版 fixture 必须声明 engine 与 source_file",
                )?;
            }
            Method::ExactWriter => {
                fail_if(
                    self.source.generator.as_deref() != Some(exact::GENERATOR),
                    "§2.5",
                    &format!("精确 fixture 必须钉死 generator = {:?}", exact::GENERATOR),
                )?;
                fail_if(
                    self.source.engine.is_some() || self.source.source_file.is_some(),
                    "§2.5",
                    "精确 fixture 由内置 recipe 生成，不应声明外部 engine/source_file",
                )?;
                fail_if(
                    self.source.font_provenance != FontProvenance::Vendored,
                    "§2.6",
                    "精确 fixture 必须嵌入钉死的字体文件（font_provenance = \"vendored\"）",
                )?;
                fail_if(
                    self.source.fonts.is_empty(),
                    "§2.6",
                    "精确 fixture 必须逐个列出字体文件及其 SHA-256",
                )?;
                for font in &self.source.fonts {
                    fail_if(
                        font.pdf_object.is_none()
                            || font.descriptor_object.is_none()
                            || font.subset_tag.as_deref().is_none_or(str::is_empty)
                            || font.base_name.as_deref().is_none_or(str::is_empty),
                        "§2.6",
                        "精确字体必须钉死 PDF font object、descriptor object、subset tag 与 base name",
                    )?;
                }
            }
            Method::ByteMutation => {
                fail_if(
                    self.lineage.is_none(),
                    "§2.5",
                    "字节变异 fixture 必须声明 [lineage]",
                )?;
            }
        }

        fail_if(
            self.source.font_provenance == FontProvenance::Vendored && self.source.fonts.is_empty(),
            "§2.6",
            "font_provenance = \"vendored\" 却没列出任何字体文件",
        )?;
        fail_if(
            self.source.pdf_sha256.len() != 64,
            "§2.6",
            "pdf_sha256 必须是 64 位十六进制",
        )
    }

    fn validate_pages(&self) -> Result<()> {
        fail_if(
            self.page.is_empty(),
            "§2.7",
            "缺少 [[page]]——逐页 MediaBox / CropBox / Rotate 是必备字段",
        )?;
        for (i, page) in self.page.iter().enumerate() {
            fail_if(
                page.index != i,
                "§2.7",
                &format!(
                    "[[page]] 的 index 必须从 0 起连续，第 {i} 项写的是 {}",
                    page.index
                ),
            )?;
            fail_if(
                page.media_box[0] >= page.media_box[2] || page.media_box[1] >= page.media_box[3],
                "§2.2",
                &format!("第 {i} 页 MediaBox 退化：{:?}", page.media_box),
            )?;
        }
        fail_if(
            self.page.len() != self.expected.structure.pages,
            "§2.7",
            &format!(
                "[[page]] 有 {} 项，expected.structure.pages 写的是 {}",
                self.page.len(),
                self.expected.structure.pages
            ),
        )
    }

    fn validate_expected(&self) -> Result<()> {
        let blocks = &self.expected.block;
        fail_if(
            blocks.len() != self.expected.structure.blocks,
            "§2.7",
            &format!(
                "[[expected.block]] 有 {} 项，expected.structure.blocks 写的是 {}",
                blocks.len(),
                self.expected.structure.blocks
            ),
        )?;
        fail_if(
            self.expected.tolerance_pt <= 0.0,
            "§2.2",
            "expected.tolerance_pt 必须为正",
        )?;
        fail_if(
            self.expected.ink_margin_pt.is_some_and(|v| v <= 0.0),
            "§2.2",
            "expected.ink_margin_pt 若给出则必须为正",
        )?;
        fail_if(
            self.expected
                .visual_tolerance_pt
                .is_some_and(|value| value <= 0.0),
            "§2.2",
            "expected.visual_tolerance_pt 若给出则必须为正",
        )?;

        // 绘制序与阅读序都必须是 1..=n 的一个排列——重号或跳号会让「顺序不同」
        // 这类断言退化成无意义的比较。
        check_permutation(
            blocks.iter().map(|b| b.draw_order),
            blocks.len(),
            "draw_order",
        )?;
        check_permutation(
            blocks.iter().map(|b| b.reading_order),
            blocks.len(),
            "reading_order",
        )?;

        // 栏数与块的 column 标注必须自洽。写错的一方在「双栏 fixture 其实只画了
        // 一栏」这类错误里是唯一的告警。
        let columns: BTreeSet<&str> = blocks
            .iter()
            .map(|b| b.column.as_str())
            .filter(|c| *c != "full")
            .collect();
        let declared = self.expected.structure.columns;
        fail_if(
            columns.is_empty() && declared != 1,
            "§2.7",
            &format!("所有块都标为 full，structure.columns 应为 1，实际写的是 {declared}"),
        )?;
        fail_if(
            !columns.is_empty() && columns.len() != declared,
            "§2.7",
            &format!(
                "块上出现了 {} 种栏标注 {columns:?}，structure.columns 写的是 {declared}",
                columns.len()
            ),
        )?;

        let mut keys = BTreeSet::new();
        for block in blocks {
            fail_if(
                !keys.insert(block.key.as_str()),
                "§2.7",
                &format!("block key `{}` 重复", block.key),
            )?;
            fail_if(
                block.page >= self.page.len(),
                "§2.7",
                &format!("block `{}` 的 page = {} 超出页数", block.key, block.page),
            )?;
            fail_if(
                block.text.trim().is_empty(),
                "§2.1",
                &format!("block `{}` 的 text 为空——结构化期望必须手写", block.key),
            )?;

            if self.expected.geometry_source == GeometrySource::HandWritten {
                fail_if(
                    block.baseline_origin.is_none()
                        || block.metric_box.is_none()
                        || block.visual_bbox.is_none(),
                    "§2.2",
                    &format!(
                        "block `{}`：手写几何必须分别给出 baseline_origin / metric_box / visual_bbox 三个盒子，不得混用",
                        block.key
                    ),
                )?;
            }
        }

        // §2.1 的裁定例外**只对现实排版 fixture** 开放。精确与畸形 fixture 若也
        // 用它，就等于把期望值交回给生成器，第一原则当场失效。
        fail_if(
            self.expected.geometry_source == GeometrySource::DualParserAdjudicated
                && self.source.method != Method::RealisticTypesetting,
            "§2.1",
            "双解析器裁定是现实排版 fixture 的**唯一例外**；其余 fixture 的几何期望必须手写",
        )?;

        if self.source.method == Method::ExactWriter {
            self.validate_exact_contract()?;
        }

        fail_if(
            self.expected.behaviour.is_empty(),
            "§2.9",
            "expected.behaviour 为空——fixture 必须声明独立可观察的期望行为",
        )?;

        let malformed = self.identity.legality == Legality::Malformed;
        fail_if(
            malformed && self.expected.declared_failure.is_none(),
            "§2.8",
            "畸形 fixture 必须声明 expected.declared_failure——「失败了」不够，得是**声明的那种**失败",
        )?;
        fail_if(
            !malformed && self.expected.declared_failure.is_some(),
            "§2.8",
            "合法 fixture 不应声明 declared_failure",
        )
    }

    fn validate_exact_contract(&self) -> Result<()> {
        for (check, name) in [
            (Check::Determinism, "determinism"),
            (Check::Legality, "legality"),
            (Check::PageGeometry, "page-geometry"),
            (Check::Structure, "structure"),
            (Check::PdfBytes, "pdf-bytes"),
            (Check::PdfStructure, "pdf-structure"),
            (Check::Render, "render"),
        ] {
            fail_if(
                !self.requires(check),
                "§2.8",
                &format!("exact-writer fixture 必须启用 {name} 门禁"),
            )?;
        }
        fail_if(
            !self.requires(Check::HandWrittenGeometry) && !self.requires(Check::Type3Geometry),
            "§2.8",
            "exact-writer fixture 必须启用 hand-written-geometry 或 type3-geometry 门禁",
        )?;

        fail_if(
            self.expected.geometry_source != GeometrySource::HandWritten,
            "§2.1",
            "exact-writer fixture 的三种盒子必须先行手写",
        )?;
        fail_if(
            self.expected.tolerance_pt > 0.001,
            "§2.2",
            "exact baseline/metric tolerance 不得超过 0.001 pt",
        )?;
        fail_if(
            self.expected
                .visual_tolerance_pt
                .is_none_or(|value| value > 0.01),
            "§2.2",
            "exact visual bbox 必须声明不超过 0.01 pt 的独立容差",
        )?;
        let pdf = self
            .expected
            .pdf
            .as_ref()
            .context("[§2.5] exact-writer fixture 缺少 [expected.pdf]")?;
        fail_if(
            pdf.version != "1.7",
            "§2.5",
            "精确 writer 当前只生成 PDF 1.7",
        )?;
        fail_if(
            pdf.header_prefix_hex.len() % 2 != 0
                || !pdf.header_prefix_hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "§2.5",
            "expected.pdf.header_prefix_hex 必须是偶数位十六进制",
        )?;
        fail_if(
            pdf.trailer_id_hex.len() != 32
                || !pdf.trailer_id_hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "§2.6",
            "trailer_id_hex 必须是 16 字节（32 位十六进制）",
        )?;
        fail_if(
            pdf.trailer_id_hex != hash::trailer_id_hex(self.id()),
            "§2.6",
            "trailer_id_hex 不符合 fixture ID 截断/补零规则",
        )?;
        let expected_numbers: Vec<u32> = (1..=pdf.object_numbers.len() as u32).collect();
        fail_if(
            pdf.object_numbers != expected_numbers,
            "§2.5",
            "exact writer 的 object_numbers 必须从 1 起连续且按写出顺序声明",
        )?;
        fail_if(
            !pdf.object_numbers.contains(&pdf.root_object),
            "§2.5",
            "root_object 不在 object_numbers 中",
        )?;
        fail_if(
            pdf.info_dictionary || pdf.metadata_streams != 0,
            "§2.6",
            "精确 fixture 禁止 Info 字典与 metadata stream",
        )?;
        fail_if(
            self.expected.content_stream.is_empty(),
            "§2.5",
            "exact-writer fixture 必须手写至少一个 content stream",
        )?;
        for stream in &self.expected.content_stream {
            fail_if(
                !pdf.object_numbers.contains(&stream.object),
                "§2.5",
                &format!("content stream object {} 不在对象计划中", stream.object),
            )?;
            fail_if(
                stream.bytes.is_empty() == stream.bytes_hex.is_none(),
                "§2.1",
                "content stream 必须且只能用 bytes / bytes_hex 之一手写 raw bytes",
            )?;
            fail_if(
                stream.compressed
                    && (stream.filters.is_empty()
                        || (stream.decoded_bytes.is_none() == stream.decoded_bytes_hex.is_none())),
                "§2.1/§2.5",
                "带过滤器的精确 stream 必须声明 filters，且只能用 decoded_bytes / decoded_bytes_hex 之一声明解码结果",
            )?;
            fail_if(
                !stream.compressed
                    && (!stream.filters.is_empty()
                        || stream.decoded_bytes.is_some()
                        || stream.decoded_bytes_hex.is_some()),
                "§2.5",
                "未过滤 stream 不应声明 filters 或 decoded bytes",
            )?;
        }
        for reference in &self.expected.reference {
            fail_if(
                !pdf.object_numbers.contains(&reference.from_object)
                    || !pdf.object_numbers.contains(&reference.to_object),
                "§2.5",
                &format!(
                    "引用 {} -> {} 超出对象计划",
                    reference.from_object, reference.to_object
                ),
            )?;
            fail_if(
                reference.path.is_empty() || reference.path.iter().any(|part| part.is_empty()),
                "§2.5",
                "对象引用 path 不得为空",
            )?;
        }
        for font in &self.source.fonts {
            let object = font
                .pdf_object
                .context("[§2.6] exact font missing pdf_object")?;
            let descriptor = font
                .descriptor_object
                .context("[§2.6] exact font missing descriptor_object")?;
            fail_if(
                !pdf.object_numbers.contains(&object) || !pdf.object_numbers.contains(&descriptor),
                "§2.6",
                "精确字体的 PDF object/descriptor object 超出对象计划",
            )?;
        }

        let structure = &self.expected.structure;
        fail_if(
            self.expected.bookmark.len() != structure.bookmarks,
            "§2.7",
            "bookmark 明细数与 expected.structure.bookmarks 不一致",
        )?;
        let depth = self
            .expected
            .bookmark
            .iter()
            .map(|bookmark| bookmark.level)
            .max()
            .unwrap_or(0);
        fail_if(
            depth != structure.bookmark_depth,
            "§2.7",
            "bookmark 最大层级与 expected.structure.bookmark_depth 不一致",
        )?;
        fail_if(
            self.expected.annotation.len() != structure.annotations,
            "§2.7",
            "annotation 明细数与 expected.structure.annotations 不一致",
        )?;
        let form_fields = self
            .expected
            .annotation
            .iter()
            .filter(|annotation| annotation.subtype == AnnotationSubtype::Widget)
            .count();
        fail_if(
            form_fields != structure.form_fields,
            "§2.7",
            "widget 明细数与 expected.structure.form_fields 不一致",
        )?;
        fail_if(
            self.expected.optional_content_group.len() != structure.optional_content_groups,
            "§2.7",
            "OCG 明细数与 expected.structure.optional_content_groups 不一致",
        )?;
        fail_if(
            self.expected.named_destination.len() != structure.named_destinations,
            "§2.7",
            "named destination 明细数与 expected.structure.named_destinations 不一致",
        )?;
        let uri_actions = self
            .expected
            .bookmark
            .iter()
            .filter(|bookmark| bookmark.target == BookmarkTarget::Uri)
            .count()
            + self
                .expected
                .annotation
                .iter()
                .filter(|annotation| annotation.uri.is_some())
                .count();
        fail_if(
            uri_actions != structure.uri_actions,
            "§2.7",
            "URI action 明细数与 expected.structure.uri_actions 不一致",
        )?;
        for bookmark in &self.expected.bookmark {
            fail_if(
                !pdf.object_numbers.contains(&bookmark.object)
                    || !pdf.object_numbers.contains(&bookmark.parent_object),
                "§2.5",
                "bookmark object/parent_object 超出对象计划",
            )?;
            fail_if(
                bookmark.level == 0 || bookmark.title.is_empty() || bookmark.style_flags > 3,
                "§2.7",
                "bookmark 的 level/title/style_flags 无效",
            )?;
            let target_valid = match bookmark.target {
                BookmarkTarget::Xyz => {
                    bookmark.page_object.is_some()
                        && bookmark.xyz.is_some()
                        && bookmark.name.is_none()
                        && bookmark.uri.is_none()
                }
                BookmarkTarget::Named => {
                    bookmark.name.is_some()
                        && bookmark.page_object.is_none()
                        && bookmark.xyz.is_none()
                        && bookmark.uri.is_none()
                }
                BookmarkTarget::Uri => {
                    bookmark.uri.is_some()
                        && bookmark.page_object.is_none()
                        && bookmark.xyz.is_none()
                        && bookmark.name.is_none()
                }
            };
            fail_if(!target_valid, "§2.7", "bookmark target 字段组合不自洽")?;
        }
        for annotation in &self.expected.annotation {
            fail_if(
                !pdf.object_numbers.contains(&annotation.object)
                    || annotation.rect[0] >= annotation.rect[2]
                    || annotation.rect[1] >= annotation.rect[3],
                "§2.7",
                "annotation object 或 rect 无效",
            )?;
            fail_if(
                annotation.subtype == AnnotationSubtype::Widget
                    && annotation.field_name.as_deref().is_none_or(str::is_empty),
                "§2.7",
                "widget annotation 必须声明 field_name",
            )?;
        }
        Ok(())
    }
}

fn check_permutation(values: impl Iterator<Item = usize>, len: usize, field: &str) -> Result<()> {
    let set: BTreeSet<usize> = values.collect();
    let expected: BTreeSet<usize> = (1..=len).collect();
    fail_if(
        set != expected,
        "§2.7",
        &format!("{field} 必须是 1..={len} 的一个排列，实际是 {set:?}"),
    )
}

fn fail_if(condition: bool, clause: Clause, message: &str) -> Result<()> {
    if condition {
        bail!("[{clause}] {message}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
schema_version = 1

[identity]
id = "unit-demo-01-sample"
name = "示例"
class = "unit"
legality = "legal"
cases = ["ORDER-01"]
priority = "M0"
variable = "示例变量"

[source]
method = "realistic-typesetting"
engine = "typst"
source_file = "unit-demo-01-sample.typ"
pdf = "unit-demo-01-sample.pdf"
pdf_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
font_provenance = "typst-embedded"

[[page]]
index = 0
media_box = [0.0, 0.0, 400.0, 300.0]
rotate = 0

[expected]
geometry_source = "dual-parser-adjudicated"
tolerance_pt = 0.05

[expected.structure]
pages = 1
blocks = 1
columns = 1

[[expected.block]]
key = "L1"
page = 0
column = "full"
draw_order = 1
reading_order = 1
text = "hello"

[[expected.behaviour]]
id = "demo"
assertion = "断言"
observable_via = "手段"

[oracle]
checks = ["determinism", "structure"]
"#;

    fn parse(toml_text: &str) -> Result<Manifest> {
        let mut m: Manifest = toml::from_str(toml_text)?;
        m.dir = PathBuf::from("unit-demo-01-sample");
        m.validate()?;
        Ok(m)
    }

    #[test]
    fn accepts_a_well_formed_manifest() {
        parse(BASE).unwrap();
    }

    #[test]
    fn rejects_a_directory_name_that_disagrees_with_the_id() {
        let mut m: Manifest = toml::from_str(BASE).unwrap();
        m.dir = PathBuf::from("some-other-name");
        let err = m.validate().unwrap_err().to_string();
        assert!(err.contains("§2.11"), "{err}");
    }

    #[test]
    fn rejects_dual_parser_adjudication_outside_realistic_typesetting() {
        let text = BASE
            .replace(
                "method = \"realistic-typesetting\"\nengine = \"typst\"\nsource_file = \"unit-demo-01-sample.typ\"",
                "method = \"exact-writer\"\ngenerator = \"corpus-exact-writer-v1\"",
            )
            .replace(
                "font_provenance = \"typst-embedded\"",
                "font_provenance = \"vendored\"\nfonts = [{ file = \"f.ttf\", sha256 = \"x\", pdf_object = 1, descriptor_object = 2, subset_tag = \"DEMOAA\", base_name = \"Demo\" }]",
            );
        let err = parse(&text).unwrap_err().to_string();
        assert!(err.contains("§2.1"), "{err}");
    }

    #[test]
    fn requires_lineage_for_malformed_fixtures() {
        let text = BASE
            .replace(
                "id = \"unit-demo-01-sample\"",
                "id = \"mal-demo-01-sample\"",
            )
            .replace("class = \"unit\"", "class = \"mal\"")
            .replace("legality = \"legal\"", "legality = \"malformed\"");
        let mut m: Manifest = toml::from_str(&text).unwrap();
        m.dir = PathBuf::from("mal-demo-01-sample");
        let err = m.validate().unwrap_err().to_string();
        assert!(err.contains("§2.5"), "{err}");
    }

    #[test]
    fn requires_all_three_boxes_when_geometry_is_hand_written() {
        let text = BASE.replace(
            "geometry_source = \"dual-parser-adjudicated\"",
            "geometry_source = \"hand-written\"",
        );
        let err = parse(&text).unwrap_err().to_string();
        assert!(err.contains("§2.2"), "{err}");
    }

    #[test]
    fn rejects_duplicate_draw_order() {
        let text = BASE
            .replace(
                "[[expected.behaviour]]",
                r#"[[expected.block]]
key = "L2"
page = 0
column = "full"
draw_order = 1
reading_order = 2
text = "world"

[[expected.behaviour]]"#,
            )
            .replace("blocks = 1", "blocks = 2");
        let err = parse(&text).unwrap_err().to_string();
        assert!(err.contains("draw_order"), "{err}");
    }

    #[test]
    fn rejects_a_block_count_that_disagrees_with_structure() {
        let text = BASE.replace("blocks = 1", "blocks = 2");
        let err = parse(&text).unwrap_err().to_string();
        assert!(err.contains("structure.blocks"), "{err}");
    }

    #[test]
    fn the_ink_margin_defaults_to_the_agreement_tolerance() {
        let m = parse(BASE).unwrap();
        assert_eq!(m.expected.ink_margin(), m.expected.tolerance_pt);
        let wider = BASE.replace(
            "tolerance_pt = 0.05",
            "tolerance_pt = 0.05\nink_margin_pt = 0.3",
        );
        assert_eq!(parse(&wider).unwrap().expected.ink_margin(), 0.3);
    }

    #[test]
    fn rejects_a_fixture_whose_hand_written_text_nothing_checks() {
        let text = BASE.replace(
            "checks = [\"determinism\", \"structure\"]",
            "checks = [\"determinism\"]",
        );
        let err = parse(&text).unwrap_err().to_string();
        assert!(err.contains("structure 或 glyphs"), "{err}");
    }

    #[test]
    fn rejects_an_empty_oracle_list() {
        let text = BASE.replace("checks = [\"determinism\", \"structure\"]", "checks = []");
        let err = parse(&text).unwrap_err().to_string();
        assert!(err.contains("§2.8"), "{err}");
    }

    #[test]
    fn rejects_a_page_index_gap() {
        let text = BASE.replace("index = 0", "index = 1");
        let err = parse(&text).unwrap_err().to_string();
        assert!(err.contains("index"), "{err}");
    }

    #[test]
    fn accepts_the_hand_written_exact_baseline_contract() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line");
        let manifest = Manifest::load(&dir).unwrap();
        assert_eq!(manifest.id(), "unit-base-01-single-line");
        assert_eq!(manifest.source.method, Method::ExactWriter);
        assert_eq!(
            manifest.expected.geometry_source,
            GeometrySource::HandWritten
        );
    }

    #[test]
    fn exact_baseline_requires_executable_font_traceability() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line");
        let mut manifest = Manifest::load(&dir).unwrap();
        manifest.source.fonts[0].subset_tag = None;

        let error = manifest.validate().unwrap_err().to_string();

        assert!(error.contains("subset tag"), "{error}");
    }

    #[test]
    fn exact_baseline_requires_structure_gate_even_when_every_count_is_zero() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line");
        let mut manifest = Manifest::load(&dir).unwrap();
        manifest
            .oracle
            .checks
            .retain(|check| *check != Check::PdfStructure);

        let error = manifest.validate().unwrap_err().to_string();

        assert!(error.contains("pdf-structure"), "{error}");
    }

    #[test]
    fn exact_baseline_requires_every_independent_acceptance_gate() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line");

        for required in [
            Check::Determinism,
            Check::Legality,
            Check::PageGeometry,
            Check::Structure,
            Check::HandWrittenGeometry,
            Check::PdfBytes,
            Check::PdfStructure,
            Check::Render,
        ] {
            let mut manifest = Manifest::load(&dir).unwrap();
            manifest.oracle.checks.retain(|check| *check != required);

            assert!(
                manifest.validate().is_err(),
                "exact manifest accepted without {required:?}"
            );
        }
    }
}
