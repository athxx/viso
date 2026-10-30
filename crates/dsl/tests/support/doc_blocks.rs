//! The fenced code blocks of the DSL documents.

const DOCS: [(&str, &str); 5] = [
    (
        "Viso_DSL_1.0.md",
        include_str!("../../../../Viso_DSL_1.0.md"),
    ),
    ("Viso_DSL_UI.md", include_str!("../../../../Viso_DSL_UI.md")),
    (
        "Viso_DSL_Game.md",
        include_str!("../../../../Viso_DSL_Game.md"),
    ),
    (
        "Viso_DSL_Implementation.md",
        include_str!("../../../../Viso_DSL_Implementation.md"),
    ),
    (
        "Viso_DSL_Rationale.md",
        include_str!("../../../../Viso_DSL_Rationale.md"),
    ),
];

/// One fenced block: its info string, the line it opens on, its body, and
/// whether it sits in the lexical part of the specification.
pub struct Block {
    pub file: &'static str,
    pub line: usize,
    pub info: &'static str,
    pub body: String,
    pub lexical: bool,
}

fn blocks(file: &'static str, text: &'static str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut open: Option<(usize, &str, String, bool)> = None;
    let mut part = "";
    for (n, line) in text.lines().enumerate() {
        if open.is_none() && line.starts_with("# ") {
            part = line;
        }
        let fence = line.trim_start();
        match &mut open {
            None if fence.starts_with("```") => {
                let info = fence.trim_start_matches('`').trim();
                open = Some((n + 1, info, String::new(), part.contains("词法规范")));
            }
            Some(_) if fence.starts_with("```") => {
                let (line, info, body, lexical) = open.take().unwrap();
                out.push(Block {
                    file,
                    line,
                    info,
                    body,
                    lexical,
                });
            }
            Some((_, _, body, _)) => {
                body.push_str(line);
                body.push('\n');
            }
            None => {}
        }
    }
    assert!(open.is_none(), "{file}: an unclosed code fence");
    out
}

/// Every block of the DSL documents, the specification first.
pub fn all_blocks() -> Vec<Block> {
    DOCS.iter()
        .flat_map(|&(file, text)| blocks(file, text))
        .collect()
}
