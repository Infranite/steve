fn character_end(source: &str, at: usize) -> Option<usize> {
    static CHAR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = CHAR.get_or_init(|| {
        regex::Regex::new(r"^'(?:\\(?:u\{[[:xdigit:]_]+\}|x[[:xdigit:]]{2}|.)|[^'\\\r\n])'")
            .unwrap()
    });
    re.find(&source[at..]).map(|m| at + m.end())
}
fn literals(source: &str) -> Vec<&str> {
    let b = source.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            i += b[i..]
                .iter()
                .position(|x| *x == b'\n')
                .unwrap_or(b.len() - i);
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if b[i] == b'r' {
            let start = i + 1;
            let mut end = start;
            while end < b.len() && b[end] == b'#' {
                end += 1;
            }
            if end < b.len() && b[end] == b'"' {
                let terminator = format!("\"{}", "#".repeat(end - start));
                let text = &source[end + 1..];
                if let Some(len) = text.find(&terminator) {
                    result.push(&text[..len]);
                    i = end + 1 + len + terminator.len();
                } else {
                    i = b.len();
                }
            } else {
                i += 1;
            }
        } else if b[i] == b'\'' {
            i = character_end(source, i).unwrap_or(i + 1);
        } else if b[i] == b'"' {
            let start = i + 1;
            i = start;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                } else if b[i] == b'"' {
                    result.push(&source[start..i]);
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    result
}
fn joined_literals(source: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut end = 0;
    let mut nesting = Vec::new();
    let mut token = String::new();
    for literal in literals(source) {
        let start = literal.as_ptr() as usize - source.as_ptr() as usize;
        let gap = &source[end..start];
        let bytes = gap.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(b"//") {
                i += bytes[i..]
                    .iter()
                    .position(|c| *c == b'\n')
                    .unwrap_or(bytes.len() - i);
            } else if bytes[i..].starts_with(b"/*") {
                i += 2;
                let mut depth = 1;
                while i < bytes.len() && depth > 0 {
                    if bytes[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if bytes[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            } else if bytes[i] == b'\'' {
                i = character_end(gap, i).unwrap_or(i + 1);
            } else {
                let c = bytes[i] as char;
                if c.is_ascii_alphanumeric() || c == '_' || c == '!' {
                    token.push(c);
                } else if c == '(' {
                    nesting.push(token == "concat!");
                    token.clear();
                } else if c == ')' {
                    nesting.pop();
                    token.clear();
                } else if !c.is_whitespace() {
                    token.clear();
                }
                i += 1;
            }
        }
        let adjacent = !out.is_empty()
            && (!gap.contains(',') || nesting.contains(&true))
            && source[end..start]
                .chars()
                .all(|c| c.is_whitespace() || r##""#rb,+()"##.contains(c));
        if adjacent {
            out.last_mut().unwrap().1.push_str(literal);
        } else {
            out.push((start, literal.to_owned()));
        }
        end = start + literal.len();
    }
    out
}
pub fn groups(source: &str) -> Vec<String> {
    joined_literals(source)
        .into_iter()
        .map(|(_, text)| text)
        .collect()
}

fn masked(source: &str) -> String {
    let mut code = source.as_bytes().to_vec();
    for text in literals(source) {
        let at = text.as_ptr() as usize - source.as_ptr() as usize;
        code[at..at + text.len()].fill(b' ');
    }
    let mut i = 0;
    while i + 1 < code.len() {
        let start = i;
        if code[i..].starts_with(b"//") {
            while i < code.len() && code[i] != b'\n' {
                i += 1;
            }
            code[start..i].fill(b' ');
        } else if code[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i + 1 < code.len() && depth > 0 {
                if code[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if code[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            code[start..i].fill(b' ');
        } else {
            i += 1;
        }
    }
    let mut code = String::from_utf8(code).unwrap();
    let mut chars = Vec::new();
    let mut at = 0;
    while at < code.len() {
        if code.as_bytes()[at] == b'\''
            && let Some(end) = character_end(&code, at)
        {
            chars.push(at..end);
            at = end;
        } else {
            at += code[at..].chars().next().unwrap().len_utf8();
        }
    }
    for range in chars {
        code.replace_range(range.clone(), &" ".repeat(range.len()));
    }
    code
}

// Items, fields and match arms have different terminators. Never swallow their parent's
// closing delimiter: in particular a cfg(test) last field must not hide a later impl.
fn skipped_end(code: &str, start: usize) -> usize {
    let mut stack = Vec::new();
    for (offset, b) in code.as_bytes()[start..].iter().enumerate() {
        let at = start + offset;
        match b {
            b'{' => stack.push(b'}'),
            b'(' => stack.push(b')'),
            b'[' => stack.push(b']'),
            b'}' | b')' | b']' => {
                if stack.last() != Some(b) {
                    return at;
                }
                stack.pop();
                if stack.is_empty() && *b == b'}' {
                    return at + 1;
                }
            }
            b',' | b';' if stack.is_empty() => return at + 1,
            _ => {}
        }
    }
    code.len()
}
pub fn production(source: &str) -> String {
    let code = masked(source);
    let attrs = regex::Regex::new(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]").unwrap();
    let mut result = source.as_bytes().to_vec();
    for attr in attrs.find_iter(&code) {
        result[attr.start()..skipped_end(&code, attr.end())].fill(b' ');
    }
    String::from_utf8(result).unwrap()
}

fn arguments(source: &str) -> Vec<&str> {
    let mask = masked(source);
    let mut stack = Vec::new();
    let mut start = 0;
    let mut out = Vec::new();
    for (at, b) in mask.bytes().enumerate() {
        match b {
            b'(' | b'[' | b'{' => stack.push(b),
            b')' | b']' | b'}' => {
                stack.pop();
            }
            b',' if stack.is_empty() => {
                out.push(source[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    if !source[start..].trim().is_empty() {
        out.push(source[start..].trim());
    }
    out
}
fn constant(source: &str, file: &std::path::Path) -> Result<String, String> {
    let source = source.trim();
    // Macro parameters name a family of files. Conservatively inspect every matching file,
    // rather than treating a macro body as an exemption from the ownership check.
    if source.starts_with('$') {
        return Ok("*".into());
    }
    if source.starts_with('"') {
        return serde_json::from_str(source).map_err(|e| format!("invalid include string: {e}"));
    }
    if source.starts_with('r') {
        return literals(source)
            .first()
            .map(|s| s.to_string())
            .ok_or("invalid raw include string".into());
    }
    let (name, body) = source
        .split_once('(')
        .ok_or("unsupported include expression")?;
    let body = body
        .trim_end()
        .strip_suffix(')')
        .ok_or("unclosed include expression")?;
    let args = arguments(body);
    match name.trim() {
        "concat!" => args
            .into_iter()
            .map(|a| constant(a, file))
            .collect::<Result<Vec<_>, _>>()
            .map(|parts| parts.concat()),
        "env!" => {
            let key = constant(args.first().ok_or("empty env!")?, file)?;
            if key != "CARGO_MANIFEST_DIR" {
                return Err(format!("unsupported include env!: {key}"));
            }
            file.ancestors()
                .find(|p| p.join("Cargo.toml").is_file())
                .map(|p| p.display().to_string())
                .ok_or("missing caller manifest".into())
        }
        _ => Err(format!("unsupported include macro: {name}")),
    }
}
pub fn includes(source: &str, file: &std::path::Path) -> Vec<std::path::PathBuf> {
    let code = masked(source);
    let re = regex::Regex::new(r"\binclude_str\s*!\s*\(").unwrap();
    re.find_iter(&code)
        .flat_map(|m| {
            let start = m.end();
            let mut depth = 1;
            let mut end = None;
            for (offset, b) in code.as_bytes()[start..].iter().enumerate() {
                if *b == b'(' {
                    depth += 1;
                } else if *b == b')' {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(start + offset);
                        break;
                    }
                }
            }
            let end = end.expect("unclosed include_str!");
            let name = constant(&source[start..end], file)
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            let path = file.parent().unwrap().join(name);
            if path.to_string_lossy().contains('*') {
                matching_files(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

fn matching_files(pattern: &std::path::Path) -> Vec<std::path::PathBuf> {
    let pattern = pattern.to_string_lossy();
    let prefix = pattern.split('*').next().unwrap();
    let base = if prefix.ends_with('/') {
        std::path::Path::new(prefix)
    } else {
        std::path::Path::new(prefix).parent().unwrap()
    };
    let re = regex::Regex::new(&format!(
        "^{}$",
        regex::escape(&pattern).replace("\\*", ".*")
    ))
    .unwrap();
    fn walk(dir: &std::path::Path, re: &regex::Regex, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if !["target", ".git"].contains(&path.file_name().unwrap().to_str().unwrap()) {
                    walk(&path, re, out);
                }
            } else if re.is_match(&path.to_string_lossy()) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(base, &re, &mut out);
    out.sort();
    out
}
