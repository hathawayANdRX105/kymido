//! format_tool_output 渲染前规整（ainotation 波2 #4：bash 工具结果曾以转义
//! 文本原样直出）。本地不跑，CI 执行（见 bin/web 与 crates 的现有测试约定）。

use web_ui::components::chat::format_tool_output;

/// 转义换行必须还原成真实换行——这是「纯文本一行流」的修复核心。
#[test]
fn escaped_newlines_become_real_lines() {
    let out = format_tool_output("bash", "# kymido\\n\\n## section");
    assert!(out.contains('\n'), "字面 \\n 未反转义为真实换行: {out:?}");
    assert!(!out.contains("\\n"), "输出仍残留字面 \\n: {out:?}");
}

/// \t \r \" \\ 一起还原；反斜杠后跟未知字符时原样保留。
#[test]
fn other_escapes_are_unescaped() {
    let out = format_tool_output("bash", "a\\tb\\r\"c\\\"d\\\\e\\x");
    assert_eq!(out, "a\tb\rc\"d\\e\\x");
}

/// 反转义与工具类型无关：bash/read/glob 同一输入同一输出（diff 高亮是
/// ToolLine 逐行的事，不在这层）。
#[test]
fn kind_does_not_change_output() {
    let raw = "left\\nright";
    assert_eq!(
        format_tool_output("bash", raw),
        format_tool_output("read", raw),
    );
    assert_eq!(
        format_tool_output("bash", raw),
        format_tool_output("glob", raw),
    );
}

/// 已含真实换行的输出原样返回：此时字面 \n 多半是正则/代码示例，
/// 不能反转义。
#[test]
fn real_newline_output_is_untouched() {
    let raw = "line1\nregex: \\d+\\n$";
    assert_eq!(format_tool_output("bash", raw), raw);
}
