//! 坐标系与三种盒子（docs/03-corpus-requirements.md §2.2 / §2.3）。
//!
//! poppler 与 mutool 都在**观看空间**里报坐标——原点左上、y 向下、且已经应用了
//! `/Rotate`。manifest 的期望值在**页面空间**——原点左下、y 向上、`/Rotate` 之前。
//! 这两个空间之间的换算是本模块的全部内容；混用它们正是旧语料坐标偏移的那类
//! 错误，所以换算只有这一处实现，且逐个 `/Rotate` 取值都有测试。

use anyhow::{Result, bail};

/// 页面空间中的矩形（原点左下，y 向上）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    pub fn to_array(self) -> [f64; 4] {
        [self.x0, self.y0, self.x1, self.y1]
    }

    /// 是否被 `other` 在容差内包含。
    pub fn contained_in(self, other: Self, tol: f64) -> bool {
        self.x0 >= other.x0 - tol
            && self.y0 >= other.y0 - tol
            && self.x1 <= other.x1 + tol
            && self.y1 <= other.y1 + tol
    }
}

/// 页面空间中的点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn to_array(self) -> [f64; 2] {
        [self.x, self.y]
    }
}

/// 一页的观看空间 ↔ 页面空间换算器。
#[derive(Debug, Clone, Copy)]
pub struct PageFrame {
    /// 有效框：CropBox 优先，缺失时退回 MediaBox（§2.3）。
    effective: [f64; 4],
    /// 规范化到 {0, 90, 180, 270} 的 `/Rotate`。
    rotate: i32,
}

impl PageFrame {
    pub fn new(effective: [f64; 4], rotate: i32) -> Result<Self> {
        if rotate % 90 != 0 {
            bail!("/Rotate = {rotate} 不是 90 的整数倍（§2.3）");
        }
        Ok(Self {
            effective,
            rotate: rotate.rem_euclid(360),
        })
    }

    fn width(self) -> f64 {
        self.effective[2] - self.effective[0]
    }

    fn height(self) -> f64 {
        self.effective[3] - self.effective[1]
    }

    /// 渲染器出图的像素朝向所对应的尺寸（已应用 `/Rotate`）。
    pub fn viewer_size(self) -> (f64, f64) {
        match self.rotate {
            90 | 270 => (self.height(), self.width()),
            _ => (self.width(), self.height()),
        }
    }

    /// 有效框自身的尺寸，**不应用** `/Rotate`。
    ///
    /// 实测（2026-08-21）：poppler `pdftotext -bbox-layout` 的
    /// `<page width height>` 报的是这个量——同一份 300×200 的页面，`/Rotate`
    /// 取 0/90/180/270 时它一律报 300×200，而同一份输出里的**坐标**是转过的。
    /// 也就是说该属性与坐标不在同一个空间里。渲染侧没有这个问题：
    /// `pdftoppm -r 72` 对 `/Rotate 90` 出的是 200×300 的图。
    pub fn box_size(self) -> (f64, f64) {
        (self.width(), self.height())
    }

    /// 观看空间的点 → 页面空间。
    pub fn to_page(self, vx: f64, vy: f64) -> Point {
        let [x0, y0, x1, y1] = self.effective;
        let (x, y) = match self.rotate {
            90 => (x0 + vy, y0 + vx),
            180 => (x1 - vx, y0 + vy),
            270 => (x1 - vy, y1 - vx),
            _ => (x0 + vx, y1 - vy),
        };
        Point { x, y }
    }

    /// 观看空间的矩形 → 页面空间。
    pub fn rect_to_page(self, vx0: f64, vy0: f64, vx1: f64, vy1: f64) -> Rect {
        let a = self.to_page(vx0, vy0);
        let b = self.to_page(vx1, vy1);
        Rect::new(a.x, a.y, b.x, b.y)
    }
}

/// 两个量在容差内是否一致。
pub fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: [f64; 4] = [0.0, 0.0, 400.0, 300.0];

    #[test]
    fn the_box_size_ignores_rotate_but_the_viewer_size_does_not() {
        for r in [0, 90, 180, 270] {
            let f = PageFrame::new(BOX, r).unwrap();
            assert_eq!(f.box_size(), (400.0, 300.0), "/Rotate {r}");
        }
        assert_eq!(
            PageFrame::new(BOX, 90).unwrap().viewer_size(),
            (300.0, 400.0)
        );
    }

    #[test]
    fn rotate_zero_only_flips_the_y_axis() {
        let f = PageFrame::new(BOX, 0).unwrap();
        assert_eq!(f.viewer_size(), (400.0, 300.0));
        assert_eq!(f.to_page(0.0, 0.0), Point { x: 0.0, y: 300.0 });
        assert_eq!(f.to_page(400.0, 300.0), Point { x: 400.0, y: 0.0 });
    }

    #[test]
    fn nonzero_media_box_origin_shifts_the_result() {
        // §2.3：MediaBox 原点不必是 (0,0)，而这正是一类独立的失效模式。
        let f = PageFrame::new([10.0, 10.0, 410.0, 310.0], 0).unwrap();
        assert_eq!(f.to_page(0.0, 0.0), Point { x: 10.0, y: 310.0 });
        assert_eq!(f.to_page(400.0, 300.0), Point { x: 410.0, y: 10.0 });
    }

    #[test]
    fn rotations_swap_the_viewer_size_for_quarter_turns() {
        assert_eq!(
            PageFrame::new(BOX, 90).unwrap().viewer_size(),
            (300.0, 400.0)
        );
        assert_eq!(
            PageFrame::new(BOX, 180).unwrap().viewer_size(),
            (400.0, 300.0)
        );
        assert_eq!(
            PageFrame::new(BOX, 270).unwrap().viewer_size(),
            (300.0, 400.0)
        );
    }

    /// 观看空间的左上角在四个 `/Rotate` 下分别对应页面空间的哪个角。
    /// 顺时针旋转显示，所以左上角依次是 左上 → 左下 → 右下 → 右上。
    #[test]
    fn the_viewer_origin_walks_the_page_corners_clockwise() {
        let corner = |r| PageFrame::new(BOX, r).unwrap().to_page(0.0, 0.0);
        assert_eq!(corner(0), Point { x: 0.0, y: 300.0 });
        assert_eq!(corner(90), Point { x: 0.0, y: 0.0 });
        assert_eq!(corner(180), Point { x: 400.0, y: 0.0 });
        assert_eq!(corner(270), Point { x: 400.0, y: 300.0 });
    }

    /// 每个 `/Rotate` 的换算都必须把观看空间的整页映射回同一个页面框——
    /// 这正是 GEOM-04 那五份「内容完全相同、只改 /Rotate」fixture 的判据。
    #[test]
    fn every_rotation_maps_the_full_viewer_page_back_onto_the_same_box() {
        for rotate in [0, 90, 180, 270] {
            let f = PageFrame::new(BOX, rotate).unwrap();
            let (w, h) = f.viewer_size();
            let rect = f.rect_to_page(0.0, 0.0, w, h);
            assert_eq!(rect, Rect::new(0.0, 0.0, 400.0, 300.0), "/Rotate {rotate}");
        }
    }

    #[test]
    fn negative_rotations_normalise() {
        let a = PageFrame::new(BOX, -90).unwrap();
        let b = PageFrame::new(BOX, 270).unwrap();
        assert_eq!(a.to_page(12.0, 34.0), b.to_page(12.0, 34.0));
    }

    #[test]
    fn rejects_rotations_that_are_not_quarter_turns() {
        assert!(PageFrame::new(BOX, 45).is_err());
    }

    #[test]
    fn containment_honours_the_tolerance() {
        let inner = Rect::new(10.0, 10.0, 20.0, 20.0);
        let outer = Rect::new(10.0, 10.0, 19.99, 20.0);
        assert!(!inner.contained_in(outer, 0.0));
        assert!(inner.contained_in(outer, 0.02));
    }
}
