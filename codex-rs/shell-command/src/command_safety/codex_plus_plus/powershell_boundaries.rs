use tree_sitter::Parser;

pub(super) fn split_script(script: &str) -> Option<Vec<String>> {
    let original = shlex::split(script)?;
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_powershell::LANGUAGE.into())
        .is_err()
    {
        return Some(original);
    }
    let Some(tree) = parser.parse(script, /*old_tree*/ None) else {
        return Some(original);
    };
    if tree.root_node().has_error() {
        return Some(original);
    }

    // Keep PowerShell statement boundaries that shlex otherwise consumes as whitespace.
    // Traverse iteratively, leaving command arguments and quoted source intact.
    let mut boundaries = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "command" | "string_literal" | "comment") {
            continue;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if node.kind() == "statement_list" && child.kind() != "comment" {
                boundaries.push(child.start_byte());
            }
            stack.push(child);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut words = Vec::new();
    let mut start = 0;
    for end in boundaries.into_iter().chain(std::iter::once(script.len())) {
        if start == end {
            continue;
        }
        let Some(segment) = shlex::split(&script[start..end]) else {
            return Some(original);
        };
        if !words.is_empty() {
            words.push(";".to_string());
        }
        words.extend(segment);
        start = end;
    }
    Some(words)
}

#[cfg(test)]
#[path = "powershell_boundaries_tests.rs"]
mod tests;
