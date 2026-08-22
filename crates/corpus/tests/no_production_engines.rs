//! docs/03-corpus-requirements.md §2.5 的自动守卫。
//!
//! > **不得使用 mimus 生产侧的 lopdf 或 PDFium 生成测试输入**。用被测组件生成
//! > 被测输入是循环论证——lopdf 写错的结构，语料会原样接受。
//!
//! 这条约束靠人记是靠不住的：`mimus-core` 迟早会依赖 lopdf 与 pdfium-render，
//! 那天只要有人给 `corpus` 加一行 `mimus-core.workspace = true`，语料就悄悄
//! 变成了自证。所以这里在**依赖闭包**上判定，而不是在直接依赖上。

use std::collections::{BTreeMap, BTreeSet};

/// 生产侧 crate 与引擎的 crate 名。命中即违规。
const FORBIDDEN: &[&str] = &[
    "mimus-core",
    "lopdf",
    "pdfium-render",
    "pdfium",
    "pdfium-sys",
];

/// 闭包的起点。
const ROOT: &str = "corpus";

#[test]
fn the_corpus_tool_never_reaches_a_production_pdf_engine() {
    let lock = read_lockfile();
    let graph = parse_dependencies(&lock);

    assert!(
        graph.contains_key(ROOT),
        "Cargo.lock 里没有 `{ROOT}` 包——这个测试失去了意义，先修它"
    );

    let violations = find_violations(&graph, ROOT);

    assert!(
        violations.is_empty(),
        "[§2.5] `{ROOT}` 的依赖闭包里出现了 mimus 生产侧 crate 或 PDF 引擎：{violations:?}。\n\
         语料工具必须只经由 qpdf / poppler / mutool / Typst / TeX 这些独立工具工作，\n\
         否则「被测组件生成被测输入」的循环论证就成立了。"
    );
}

#[test]
fn the_guard_would_actually_fire() {
    // 守卫本身也要能被证伪：给一个人造的依赖图，闭包必须找得到那个包。
    let graph = BTreeMap::from([
        (ROOT.to_string(), vec!["helper".to_string()]),
        ("helper".to_string(), vec!["lopdf".to_string()]),
        ("lopdf".to_string(), vec![]),
    ]);
    let reachable = closure(&graph, ROOT);
    assert!(reachable.contains("lopdf"), "闭包没能穿过一层间接依赖");
}

#[test]
fn the_guard_rejects_mimus_core_before_it_gains_pdf_dependencies() {
    let graph = BTreeMap::from([
        (ROOT.to_string(), vec!["mimus-core".to_string()]),
        ("mimus-core".to_string(), vec![]),
    ]);

    assert_eq!(find_violations(&graph, ROOT), vec!["mimus-core"]);
}

#[test]
fn dependency_entries_may_carry_a_version_suffix() {
    // Cargo.lock 在同名多版本时会写成 `"name version"`。
    let lock = r#"
[[package]]
name = "corpus"
version = "0.0.0"
dependencies = [
 "anyhow",
 "toml 0.9.12+spec-1.1.0",
]

[[package]]
name = "toml"
version = "0.9.12+spec-1.1.0"
"#;
    let graph = parse_dependencies(lock);
    assert_eq!(graph[ROOT], vec!["anyhow".to_string(), "toml".to_string()]);
}

fn read_lockfile() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Cargo.lock")
        .canonicalize()
        .expect("找不到 workspace 的 Cargo.lock");
    std::fs::read_to_string(&path).expect("读取 Cargo.lock 失败")
}

/// 从 Cargo.lock 抽出「包名 → 直接依赖名」的图。
fn parse_dependencies(lock: &str) -> BTreeMap<String, Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Lock {
        #[serde(default)]
        package: Vec<Package>,
    }
    #[derive(serde::Deserialize)]
    struct Package {
        name: String,
        #[serde(default)]
        dependencies: Vec<String>,
    }

    let parsed: Lock = toml::from_str(lock).expect("解析 Cargo.lock 失败");
    parsed
        .package
        .into_iter()
        .map(|p| {
            let deps = p
                .dependencies
                .into_iter()
                // 条目形如 `name`、`name version` 或 `name version (source)`。
                .map(|d| d.split_whitespace().next().unwrap_or_default().to_string())
                .collect();
            (p.name, deps)
        })
        .collect()
}

fn closure(graph: &BTreeMap<String, Vec<String>>, root: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        for dep in graph.get(&name).into_iter().flatten() {
            stack.push(dep.clone());
        }
    }
    seen.remove(root);
    seen
}

fn find_violations(graph: &BTreeMap<String, Vec<String>>, root: &str) -> Vec<&'static str> {
    let reachable = closure(graph, root);
    FORBIDDEN
        .iter()
        .copied()
        .filter(|name| reachable.contains(*name))
        .collect()
}
