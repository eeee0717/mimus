//! `adjudicated.toml` —— 独立工具测得的几何裁定与参考栅格哈希。
//!
//! 这份文件**不是手写的**，因此和 manifest 分开存放：manifest 是先于生成写死的
//! 规格，这里是事后测出来的观测结果。现实排版 fixture 可以按 §2.1 唯一例外记录
//! 双解析器几何；精确 fixture 的三种几何仍只在 manifest 中手写，这里只存渲染哈希。
//! 混在一起就分不清哪些数字是「说好的」、哪些是「量出来的」。

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::geom::{Point, Rect};
use crate::oracle::render::PageRaster;

pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// 复现比较的容差：只吸收浮点格式化的往返误差，不吸收真实差异。
const EPSILON: f64 = 1e-6;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adjudicated {
    pub schema_version: u32,
    pub fixture: String,
    pub tolerance_pt: f64,
    #[serde(default)]
    pub block: Vec<BlockGeometry>,
    #[serde(default)]
    pub render: Vec<RenderReference>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlockGeometry {
    pub key: String,
    pub page: usize,
    /// 现实排版裁定的字体度量盒：poppler `pdftotext -bbox-layout` 词盒并集。
    pub metric_box: [f64; 4],
    /// 现实排版裁定的近似墨迹盒：mutool `draw -F stext` 字形 quad 并集。
    pub visual_bbox: [f64; 4],
    /// 现实排版裁定的首字符绘制起点：mutool stext 的 char origin。
    pub baseline_origin: [f64; 2],
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RenderReference {
    pub page: usize,
    pub dpi: u32,
    pub poppler_sha256: String,
    /// Poppler bottles with the same version can render different PNG bytes across
    /// operating systems. Keep the original adjudication and pin the hosted Linux
    /// realization separately when it differs.
    #[serde(default)]
    pub poppler_linux_x86_64_sha256: Option<String>,
    pub mutool_md5: String,
}

impl Adjudicated {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取 {} 失败——先跑 `corpus adjudicate`", path.display()))?;
        let parsed: Self =
            toml::from_str(&text).with_context(|| format!("解析 {} 失败", path.display()))?;
        if parsed.schema_version != SUPPORTED_SCHEMA_VERSION {
            bail!(
                "{} 的 schema_version = {}，本工具只支持 {SUPPORTED_SCHEMA_VERSION}",
                path.display(),
                parsed.schema_version
            );
        }
        Ok(parsed)
    }

    /// 与一次新的裁定结果比对；不一致即说明工具、fixture 或换算发生了变化。
    pub fn differences(&self, fresh: &Adjudicated) -> Vec<String> {
        let mut out = Vec::new();

        if self.block.len() != fresh.block.len() {
            out.push(format!(
                "块数：已记录 {}，本次裁定 {}",
                self.block.len(),
                fresh.block.len()
            ));
            return out;
        }

        for (old, new) in self.block.iter().zip(&fresh.block) {
            if old.key != new.key || old.page != new.page {
                out.push(format!("块 `{}` 的身份变了：{old:?} → {new:?}", old.key));
                continue;
            }
            diff_array(
                &mut out,
                &old.key,
                "metric_box",
                &old.metric_box,
                &new.metric_box,
            );
            diff_array(
                &mut out,
                &old.key,
                "visual_bbox",
                &old.visual_bbox,
                &new.visual_bbox,
            );
            diff_array(
                &mut out,
                &old.key,
                "baseline_origin",
                &old.baseline_origin,
                &new.baseline_origin,
            );
        }

        let render_differs = self.render.len() != fresh.render.len()
            || self.render.iter().zip(&fresh.render).any(|(old, new)| {
                old.page != new.page
                    || old.dpi != new.dpi
                    || old.expected_poppler_sha256() != new.poppler_sha256
                    || old.mutool_md5 != new.mutool_md5
            });
        if render_differs {
            out.push(format!(
                "参考栅格哈希变了：已记录 {:?}，本次 {:?}",
                self.render, fresh.render
            ));
        }

        out
    }

    /// 渲染成带注释的 TOML。刻意手写而非 serde 序列化——这份文件要被人读，
    /// 每个数字的来源必须写在它旁边。
    pub fn to_toml(&self) -> String {
        let mut s = String::new();
        s.push_str("# 本文件由 `corpus adjudicate` 生成，**不要手工编辑**。\n");
        s.push_str("# 它只保存独立工具测得的结果，与先行手写的 manifest.toml 分开：\n");
        s.push_str("# 现实排版 fixture 可按 §2.1 唯一例外记录双解析器裁定的 [[block]]；\n");
        s.push_str("# 精确 fixture 的 baseline/metric/visual 三种几何仍以 manifest 为准，\n");
        s.push_str("# 本文件只为它记录独立渲染器的 [[render]] 哈希。\n#\n");
        s.push_str("#   metric_box      ← poppler pdftotext -bbox-layout（现实排版裁定）\n");
        s.push_str("#   visual_bbox     ← mutool draw -F stext 字形 quad（现实排版近似值）\n");
        s.push_str("#   baseline_origin ← mutool stext 首字符 origin（现实排版裁定）\n#\n");
        s.push_str("# 同版本 Poppler 的 PNG 在平台间不保证字节相同；差异平台另记钉死哈希。\n");
        s.push_str("# 坐标一律在 PDF 页面空间：单位 pt、原点左下、/Rotate 之前（§2.2）。\n\n");

        s.push_str(&format!("schema_version = {}\n", self.schema_version));
        s.push_str(&format!("fixture = {:?}\n", self.fixture));
        s.push_str(&format!("tolerance_pt = {}\n", fmt(self.tolerance_pt)));

        for b in &self.block {
            s.push_str("\n[[block]]\n");
            s.push_str(&format!("key = {:?}\n", b.key));
            s.push_str(&format!("page = {}\n", b.page));
            s.push_str(&format!("metric_box = {}\n", fmt_array(&b.metric_box)));
            s.push_str(&format!("visual_bbox = {}\n", fmt_array(&b.visual_bbox)));
            s.push_str(&format!(
                "baseline_origin = {}\n",
                fmt_array(&b.baseline_origin)
            ));
        }

        for r in &self.render {
            s.push_str("\n[[render]]\n");
            s.push_str(&format!("page = {}\n", r.page));
            s.push_str(&format!("dpi = {}\n", r.dpi));
            s.push_str(&format!("poppler_sha256 = {:?}\n", r.poppler_sha256));
            if let Some(sha256) = &r.poppler_linux_x86_64_sha256 {
                s.push_str(&format!("poppler_linux_x86_64_sha256 = {sha256:?}\n"));
            }
            s.push_str(&format!("mutool_md5 = {:?}\n", r.mutool_md5));
        }

        s
    }
}

impl RenderReference {
    fn expected_poppler_sha256(&self) -> &str {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            self.poppler_linux_x86_64_sha256
                .as_deref()
                .unwrap_or(&self.poppler_sha256)
        } else {
            &self.poppler_sha256
        }
    }
}

impl BlockGeometry {
    pub fn new(key: &str, page: usize, metric: Rect, visual: Rect, baseline: Point) -> Self {
        Self {
            key: key.to_string(),
            page,
            metric_box: metric.to_array(),
            visual_bbox: visual.to_array(),
            baseline_origin: baseline.to_array(),
        }
    }
}

impl From<&PageRaster> for RenderReference {
    fn from(r: &PageRaster) -> Self {
        Self {
            page: r.index,
            dpi: crate::oracle::render::DPI,
            poppler_sha256: r.poppler_sha256.clone(),
            poppler_linux_x86_64_sha256: None,
            mutool_md5: r.mutool_md5.clone(),
        }
    }
}

fn diff_array(out: &mut Vec<String>, key: &str, field: &str, old: &[f64], new: &[f64]) {
    if old.iter().zip(new).any(|(a, b)| (a - b).abs() > EPSILON) {
        out.push(format!(
            "块 `{key}` 的 {field}：已记录 {}，本次裁定 {}",
            fmt_array(old),
            fmt_array(new)
        ));
    }
}

fn fmt(v: f64) -> String {
    // 固定小数位数，不用平台默认 repr（§2.6 浮点格式化条款）。
    format!("{v:.6}")
}

fn fmt_array(v: &[f64]) -> String {
    format!(
        "[{}]",
        v.iter().map(|x| fmt(*x)).collect::<Vec<_>>().join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Adjudicated {
        Adjudicated {
            schema_version: 1,
            fixture: "unit-demo-01".into(),
            tolerance_pt: 0.05,
            block: vec![BlockGeometry {
                key: "L1".into(),
                page: 0,
                metric_box: [1.0, 2.0, 3.0, 4.0],
                visual_bbox: [1.1, 2.1, 2.9, 3.9],
                baseline_origin: [1.0, 3.5],
            }],
            render: vec![RenderReference {
                page: 0,
                dpi: 150,
                poppler_sha256: "aa".into(),
                poppler_linux_x86_64_sha256: None,
                mutool_md5: "bb".into(),
            }],
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let a = sample();
        let text = a.to_toml();
        let b: Adjudicated = toml::from_str(&text).unwrap();
        assert!(a.differences(&b).is_empty(), "{:?}", a.differences(&b));
    }

    #[test]
    fn reports_a_changed_box() {
        let a = sample();
        let mut b = sample();
        b.block[0].visual_bbox[2] = 2.5;
        let diffs = a.differences(&b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("visual_bbox"), "{diffs:?}");
    }

    #[test]
    fn reports_a_changed_raster_hash() {
        let a = sample();
        let mut b = sample();
        b.render[0].mutool_md5 = "cc".into();
        assert!(a.differences(&b).iter().any(|d| d.contains("栅格")));
    }

    #[test]
    fn linux_poppler_hash_is_additive_and_round_trips() {
        let mut a = sample();
        a.render[0].poppler_linux_x86_64_sha256 = Some("cc".into());
        let text = a.to_toml();
        assert!(text.contains("poppler_linux_x86_64_sha256 = \"cc\""));
        let b: Adjudicated = toml::from_str(&text).unwrap();
        assert_eq!(
            b.render[0].poppler_linux_x86_64_sha256.as_deref(),
            Some("cc")
        );
    }

    #[test]
    fn a_block_count_change_short_circuits_the_diff() {
        let a = sample();
        let mut b = sample();
        b.block.clear();
        let diffs = a.differences(&b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("块数"), "{diffs:?}");
    }

    #[test]
    fn float_formatting_is_fixed_width_not_platform_repr() {
        assert_eq!(fmt(1.0), "1.000000");
        assert_eq!(fmt_array(&[1.0, 2.5]), "[1.000000, 2.500000]");
    }
}
