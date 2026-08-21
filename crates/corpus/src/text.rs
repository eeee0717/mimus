//! 文本归一化——两个解析器的文本必须在同一把尺子下比较。

/// 软连字符。排版引擎在断行处插入它，两个解析器都会原样报出来。
const SOFT_HYPHEN: char = '\u{ad}';

/// 把若干**行**拼成一段可与手写期望比较的文本。
///
/// 关键在断行处：行尾若是软连字符，说明这是同一个词被拆开，必须去掉软连字符
/// 后**无缝**拼接（`sur\u{ad}` + `vive` → `survive`）；否则按一个空格拼接。
/// 忽略这一条的话，手写期望就得写成「sur vive」——那等于让期望值迁就工具，
/// 正是 §2.1 禁止的方向。
pub fn join_lines<S: AsRef<str>>(lines: &[S]) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.as_ref();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed.strip_suffix(SOFT_HYPHEN) {
            Some(head) => out.push_str(head),
            None => {
                out.push_str(trimmed);
                out.push(' ');
            }
        }
    }
    normalize(&out)
}

/// 归一化：去掉软连字符、把连续空白折成单个空格、去首尾空白。
pub fn normalize(s: &str) -> String {
    s.replace(SOFT_HYPHEN, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejoins_a_word_split_by_a_soft_hyphen() {
        let lines = ["Reading order must sur\u{ad}", "vive a permuted stream"];
        assert_eq!(
            join_lines(&lines),
            "Reading order must survive a permuted stream"
        );
    }

    #[test]
    fn joins_ordinary_line_breaks_with_a_single_space() {
        assert_eq!(join_lines(&["alpha", "beta"]), "alpha beta");
    }

    #[test]
    fn collapses_runs_of_whitespace() {
        assert_eq!(normalize("a   b\n\tc "), "a b c");
    }

    #[test]
    fn drops_soft_hyphens_that_are_not_at_a_line_end() {
        assert_eq!(normalize("co\u{ad}operate"), "cooperate");
    }

    #[test]
    fn skips_blank_lines() {
        assert_eq!(join_lines(&["a", "  ", "b"]), "a b");
    }
}
