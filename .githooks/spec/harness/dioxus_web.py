#!/usr/bin/env python3
"""Dioxus/rsx 结构与样式扫描器，被多条 checklist 复用。

一条命令输出三类 finding，调用方用 `--only` 挑：

    nesting    rsx! 块内「元素套元素」的峰值深度
    style      内联 class: 过长 / 硬编码颜色 / 原始色板类
    layout     views 目录里定义 #[component]、component 目录缺失

关于 nesting 的口径——这是全部规则的命门：
`prop: rsx! { ... }`（传 slot）、`if cond { rsx! {} }`（条件渲染）、
`.map(|x| rsx! {})`（列表渲染）都是 Dioxus 惯用法，**不算**元素嵌套。
把它们算进去，规则会天天误报惯用法，agent 学一次就学会无视这条规则。
所以这里用栈逐个花括号判类别，而不是正则数 `Name {` 的行数。

峰值必须在遍历过程中取：块结束时栈已弹平，那时再数永远是 0。
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, asdict

# ── 语法识别 ──────────────────────────────────────────────────────
RSX_START = re.compile(r'\brsx!\s*\{')
CONTROL = re.compile(r'^\s*(if|else\s+if|else|for|while|match|loop)\b')
IDENT_TAIL = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)\s*$')

NOT_ELEMENT = {
    'rsx', 'move', 'else', 'if', 'for', 'while', 'match', 'loop',
    'mut', 'ref', 'async', 'unsafe', 'impl', 'fn', 'let', 'use', 'in',
}
HTMLISH = {
    'div', 'span', 'section', 'article', 'header', 'footer', 'nav', 'main',
    'aside', 'ul', 'ol', 'li', 'table', 'form', 'button', 'a', 'p', 'h1',
    'h2', 'h3', 'h4', 'h5', 'h6', 'label', 'img', 'svg', 'details',
    'summary', 'dialog', 'pre', 'code', 'strong', 'em', 'small', 'hr',
    'input', 'select', 'option', 'textarea', 'fieldset', 'tr', 'td', 'th',
    'thead', 'tbody', 'caption', 'iframe', 'video', 'audio', 'canvas',
}

# ── 样式口径 ──────────────────────────────────────────────────────
CLASS_ATTR = re.compile(r'class:\s*"([^"]*)"')
HEX_COLOR = re.compile(r'#[0-9a-fA-F]{3,8}\b')
# 原始色板类：bg-zinc-800 / text-white / border-slate-200 …
# ui-kit 的语义 token 是 bg-card / text-foreground / border-border，不在此列。
RAW_PALETTE = re.compile(
    r'\b(?:bg|text|border|from|to|via|ring|fill|stroke|divide|outline|shadow|accent)'
    r'-(?:zinc|slate|gray|grey|neutral|stone|red|orange|amber|yellow|lime|green|emerald|'
    r'teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)-[0-9]{2,3}\b'
)
WHITE_BLACK = re.compile(r'\b(?:bg|text|border)-(?:white|black)\b')


def classify_brace(line: str) -> str:
    """判定某个 `{` 是否为「元素开括号」。"""
    if RSX_START.search(line) or CONTROL.match(line):
        return 'other'
    m = IDENT_TAIL.search(line)
    if not m:
        return 'other'
    name = m.group(1)
    if name in NOT_ELEMENT:
        return 'other'
    return 'element' if (name[0].isupper() or name in HTMLISH) else 'other'


@dataclass
class Finding:
    id: str
    severity: str
    path: str
    line: int
    message: str


def peak_nesting(src: str) -> list[tuple[int, int, int]]:
    """每个 rsx! 块的 (峰值元素深度, 起始行, 结束行)。"""
    out = []
    for m in RSX_START.finditer(src):
        start = src.count('\n', 0, m.start()) + 1
        stack = ['root']
        peak = 0
        j = m.end()                      # 已越过 rsx! 的开括号
        while j < len(src) and stack:
            c = src[j]
            if c == '{':
                ls = src.rfind('\n', 0, j) + 1
                stack.append(classify_brace(src[ls:j]))
                peak = max(peak, stack.count('element'))   # 必须在遍历中取
            elif c == '}' and len(stack) > 1:
                stack.pop()
            j += 1
        if peak:
            out.append((peak, start, src.count('\n', 0, j) + 1))
    return out


def scan_nesting(path: str, src: str, limit: int) -> list[Finding]:
    return [
        Finding(
            id="RSX-NESTED-ELEMENT",
            severity="WARN",
            path=path,
            line=start,
            message=(
                f"rsx 块内元素嵌套 {depth} 层（第 {start}-{end} 行）。"
                "Dioxus 里 slot 传片段用 `prop: rsx!{}` 是对的，这里说的是 `div{{div{{div{{}}}}}}` 这种裸元素套元素。"
                "重构：把最内层结构抽成 `#[component] pub fn X() -> Element` 放同文件的 component 区域或 "
                "crate 的 components/ 下，父层只留一次调用 + 传 props。"
            ),
        )
        for depth, start, end in peak_nesting(src)
        if depth >= limit
    ]


def scan_style(path: str, src: str, class_limit: int) -> list[Finding]:
    findings: list[Finding] = []
    for lineno, line in enumerate(src.split('\n'), 1):
        if line.lstrip().startswith('//'):
            continue
        for value in CLASS_ATTR.findall(line):
            if len(value) >= class_limit:
                findings.append(Finding(
                    id="DIOXUS-INLINE-CLASS",
                    severity="WARN",
                    path=path, line=lineno,
                    message=(
                        f"内联 class 有 {len(value)} 字符（阈值 {class_limit}）：\"{value}\"。"
                        "重构：优先用 ui_kit::styles 里已有的常量（SECTION / CARD_CONTENT / INPUT / TYPE_* / C_* / S_* / B_*）；"
                        "没有对应常量且这套样式在别处也出现，提到 ui-kit styles.rs 加常量再引用；"
                        "只此一处用的才留在原地。别把单个原子类也提成常量——ui-kit 明确禁止过度抽象。"
                    ),
                ))
        if HEX_COLOR.search(line):
            findings.append(Finding(
                id="DIOXUS-HARDCODED-COLOR",
                severity="WARN",
                path=path, line=lineno,
                message=(
                    f"硬编码颜色：{HEX_COLOR.search(line).group(0)}。"
                    "重构：换 ui-kit 的角色 token（text-foreground / text-muted-foreground / bg-card / "
                    "bg-primary / bg-destructive / border-border），或引用 ui_kit::styles 的 C_* 常量，"
                    "让主题切换只改一处。"
                ),
            ))
        raw = RAW_PALETTE.search(line) or WHITE_BLACK.search(line)
        if raw:
            findings.append(Finding(
                id="DIOXUS-RAW-PALETTE",
                severity="WARN",
                path=path, line=lineno,
                message=(
                    f"用了原始色板类 {raw.group(0)}，绕开了语义 token。"
                    "重构：换成对应角色类（bg-card / text-muted-foreground / border-border …），"
                    "这样换主题不用逐处改。"
                ),
            ))
    return findings


def scan_layering(path: str, src: str) -> list[Finding]:
    """层级约束的可判定信号是 **import 方向**，不是「组件放在哪个目录」。

    实测（omenic）：`views/config.rs` 里放 6 个 `#[component]` 是他们刻意的做法
    ——「一文件一 pub 组件 + 私有子组件同文件」。按目录一刀切会误报 6 处。
    反过来 `components/` 不得 `use crate::views::` 在现状是 0 违例，是干净硬约束。
    """
    norm = path.replace('\\', '/')
    if not re.search(r'/(components|layouts|utils)/', norm):
        return []
    for lineno, line in enumerate(src.split('\n'), 1):
        if re.search(r'\buse\s+(crate|super)::(views)::', line):
            return [Finding(
                id="WEB-LAYERING-DEPENDENCY",
                severity="FAIL",
                path=path, line=lineno,
                message=(
                    f"{norm.rsplit('/', 2)[-2]}/ 反向依赖了 views/。"
                    "层级是单向的：views → components/layouts/utils，反向不允许。"
                    "重构：把它需要的页面级状态改成 props 传入（`#[component] pub fn X(#[prop(into)] ...)`），"
                    "或把真正共用的部分下沉到 components/ 再由两边各自引用。"
                ),
            )]
    return []


def tracked_rs_files(root: str) -> list[str]:
    """用 git ls-files 取扫描集：gitignore 感知，不会扫进 target/ 与 .wt/。"""
    out = subprocess.run(
        ['git', 'ls-files', '-z', '--', '*.rs'],
        cwd=root, capture_output=True,
    )
    if not out.returncode:
        return [p for p in out.stdout.decode('utf-8', 'replace').split('\0') if p]
    import pathlib
    return [str(p) for p in pathlib.Path(root).rglob('*.rs')
            if 'target' not in p.parts and '.wt' not in p.parts]


def changed_rs_files(root: str, base: str) -> list[str]:
    """基线以来改动过的 .rs。

    存量仓（ferrite 2222 条）必须靠这个收窄到「你这次动过的文件」，
    否则一条规则一次报几千条 WARN，agent 的唯一理性反应就是忽略它——
    等于规则不存在，还多了一个假绿来源。
    """
    for spec in (f'{base}...HEAD', f'{base}..HEAD', base):
        out = subprocess.run(
            ['git', 'diff', '--name-only', '-z', spec, '--', '*.rs'],
            cwd=root, capture_output=True,
        )
        if out.returncode == 0:
            return [p for p in out.stdout.decode('utf-8', 'replace').split('\0') if p]
    return []


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--max', type=int, default=40,
                    help='单次最多输出多少条 finding（防止刷屏）；统计真实量级时传 0')
    ap.add_argument('--only', choices=['nesting', 'style', 'layering'], default=None)
    ap.add_argument('--nesting-limit', type=int, default=3)
    ap.add_argument('--scope', choices=['repo', 'changed'], default='repo',
                    help='repo=全仓审计(CI/merge 用)；changed=只看基线以来的改动(hook 热路径用)')
    ap.add_argument('--base', default=None,
                    help='changed 范围的基线 rev；缺省取 GATE_BASE，再缺省 origin/main')
    ap.add_argument('--class-limit', type=int, default=72)
    ap.add_argument('--root', default=None)
    args = ap.parse_args()

    root = args.root or subprocess.run(
        ['git', 'rev-parse', '--show-toplevel'],
        capture_output=True, text=True,
    ).stdout.strip() or '.'

    if args.scope == 'changed':
        base = args.base or os.environ.get('GATE_BASE') or 'origin/main'
        targets = changed_rs_files(root, base)
    else:
        targets = tracked_rs_files(root)

    findings: list[Finding] = []
    for rel in targets:
        try:
            src = open(f'{root}/{rel}', encoding='utf-8', errors='replace').read()
        except OSError:
            continue
        # 判据是**内容**不是路径名：按目录名过滤会让 web crate 一改名就静默失效，
        # 表现为「规则没报=通过」的假绿。带 rsx! 的文件才是 web UI 代码。
        if 'rsx!' not in src and '/views/' not in rel.replace('\\', '/'):
            continue
        if args.only in (None, 'nesting'):
            findings += scan_nesting(rel, src, args.nesting_limit)
        if args.only in (None, 'style'):
            findings += scan_style(rel, src, args.class_limit)
        if args.only in (None, 'layering'):
            findings += scan_layering(rel, src)

    findings.sort(key=lambda f: (f.path, f.line))
    total = len(findings)
    if args.max:
        findings = findings[:args.max]
    print(json.dumps([asdict(f) for f in findings], ensure_ascii=False))
    if not args.max:
        print(f'total={total}', file=sys.stderr)
    return 0

if __name__ == '__main__':
    sys.exit(main())
