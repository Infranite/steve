//! Lossless edits of literal environment assignments. This parser never evaluates shell code.
//! Keep the original bytes: unrelated values, comments and whitespace may contain private data.
use anyhow::Result;
use std::collections::BTreeMap;
use std::ops::Range;

struct Assignment {
    value: Range<usize>,
    line: usize,
}

/// Intentionally has no `Debug`: neither diagnostics nor logs should expose file contents.
pub(crate) struct EnvDocument {
    original: Vec<u8>,
    assignments: BTreeMap<String, Assignment>,
    values: BTreeMap<String, String>,
    line_ending: &'static [u8],
}

fn problem(line: Option<usize>, key: Option<&str>, reason: &str) -> anyhow::Error {
    let location = line.map_or_else(|| "环境配置更新".into(), |n| format!("环境文件第 {n} 行"));
    match key {
        Some(key) => anyhow::anyhow!("{location}（{key}）：{reason}"),
        None => anyhow::anyhow!("{location}：{reason}"),
    }
}

fn key_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn key_byte(byte: u8) -> bool {
    key_start(byte) || byte.is_ascii_digit()
}

fn valid_key(key: &str) -> bool {
    key.as_bytes().first().is_some_and(|b| key_start(*b)) && key.bytes().all(key_byte)
}

fn blank(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

fn skip_blanks(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(|b| blank(*b)) {
        at += 1;
    }
    at
}

/// Locate one shell word without evaluating it. Single-quoted bytes and escaped characters
/// remain literal; unquoted shell operators/expansions and double-quoted expansions are refused.
fn value_end(bytes: &[u8], start: usize, line: usize, key: &str) -> Result<usize> {
    let mut at = start;
    let mut quote = None;
    while let Some(&byte) = bytes.get(at) {
        match quote {
            Some(b'\'') => {
                if byte == b'\'' {
                    quote = None;
                }
            }
            Some(b'"') => match byte {
                b'"' => quote = None,
                b'$' | b'`' => {
                    return Err(problem(Some(line), Some(key), "不支持展开或命令替换"));
                }
                b'\\' => {
                    let next = bytes.get(at + 1).ok_or_else(|| {
                        problem(Some(line), Some(key), "不支持续行或未完成的转义")
                    })?;
                    // Inside double quotes POSIX shell only removes these backslashes.
                    // Other escaped characters retain the backslash and are decoded by shlex.
                    if matches!(next, b'$' | b'`' | b'"' | b'\\') {
                        at += 1;
                    }
                }
                _ => {}
            },
            None => match byte {
                b' ' | b'\t' => break,
                b'\'' | b'"' => quote = Some(byte),
                b'\\' => {
                    if bytes.get(at + 1).is_none() {
                        return Err(problem(Some(line), Some(key), "不支持续行或未完成的转义"));
                    }
                    at += 1;
                }
                // zsh can expand =command at the beginning of a path component.
                b'=' if at == start || bytes[at - 1] == b':' => {
                    return Err(problem(
                        Some(line),
                        Some(key),
                        "值包含未引用的 shell 表达式或特殊字符",
                    ));
                }
                // Do not accept shell operators, substitutions or glob/brace expansion
                // as unquoted literal data.
                b'$' | b'`' | b';' | b'&' | b'|' | b'<' | b'>' | b'(' | b')' | b'~' | b'*'
                | b'?' | b'[' | b']' | b'{' | b'}' | b'!' => {
                    return Err(problem(
                        Some(line),
                        Some(key),
                        "值包含未引用的 shell 表达式或特殊字符",
                    ));
                }
                _ => {}
            },
            _ => unreachable!("only single and double quotes are tracked"),
        }
        at += 1;
    }
    if quote.is_some() {
        return Err(problem(Some(line), Some(key), "引号未闭合；不支持跨行值"));
    }
    Ok(at)
}

impl EnvDocument {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|error| {
            let line = bytes[..error.valid_up_to()]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1;
            problem(Some(line), None, "文件必须为 UTF-8")
        })?;
        let mut document = Self {
            original: bytes.to_vec(),
            assignments: BTreeMap::new(),
            values: BTreeMap::new(),
            line_ending: if bytes
                .iter()
                .position(|&b| b == b'\n')
                .is_some_and(|at| at > 0 && bytes[at - 1] == b'\r')
            {
                b"\r\n"
            } else {
                b"\n"
            },
        };
        let mut offset = 0;
        for (index, raw) in text.split_inclusive('\n').enumerate() {
            let line = index + 1;
            let body = match raw.strip_suffix('\n') {
                Some(body) => body.strip_suffix('\r').unwrap_or(body),
                None => raw,
            };
            if body.bytes().any(|b| matches!(b, 0 | b'\r')) {
                return Err(problem(Some(line), None, "不支持 NUL 或独立回车字符"));
            }
            let bytes = body.as_bytes();
            let mut at = skip_blanks(bytes, 0);
            if at == bytes.len() || bytes[at] == b'#' {
                offset += raw.len();
                continue;
            }
            if bytes[at..].starts_with(b"export") && bytes.get(at + 6).is_some_and(|b| blank(*b)) {
                at = skip_blanks(bytes, at + 6);
            }
            let name_start = at;
            if !bytes.get(at).is_some_and(|b| key_start(*b)) {
                return Err(problem(
                    Some(line),
                    None,
                    "只支持 KEY=value 或 export KEY=value",
                ));
            }
            at += 1;
            while bytes.get(at).is_some_and(|b| key_byte(*b)) {
                at += 1;
            }
            let key = &body[name_start..at];
            if bytes.get(at) != Some(&b'=') {
                return Err(problem(
                    Some(line),
                    Some(key),
                    "变量名后必须紧接等号；不支持 shell 语句",
                ));
            }
            if document.assignments.contains_key(key) {
                return Err(problem(Some(line), Some(key), "变量重复赋值"));
            }
            let start = at + 1;
            let end = value_end(bytes, start, line, key)?;
            let rest = skip_blanks(bytes, end);
            if rest != bytes.len() && bytes[rest] != b'#' {
                return Err(problem(Some(line), Some(key), "每行只允许一个字面赋值"));
            }
            // Prefixing the word preserves an initial # as assignment data, rather than
            // letting shlex treat a valid KEY=#fragment value as a comment.
            let words = shlex::split(&format!("v={}", &body[start..end]))
                .filter(|words| words.len() == 1)
                .ok_or_else(|| problem(Some(line), Some(key), "无法解析字面值"))?;
            let value = words[0]
                .strip_prefix("v=")
                .ok_or_else(|| problem(Some(line), Some(key), "无法解析字面值"))?;
            document.values.insert(key.to_owned(), value.to_owned());
            document.assignments.insert(
                key.to_owned(),
                Assignment {
                    value: offset + start..offset + end,
                    line,
                },
            );
            offset += raw.len();
        }
        Ok(document)
    }

    pub(crate) fn values(&self) -> &BTreeMap<String, String> {
        &self.values
    }

    /// Replace only changed value spans. Append missing keys in deterministic map order,
    /// using the first existing line ending (or LF for an empty/single-line file).
    pub(crate) fn update(&self, changes: &BTreeMap<String, String>) -> Result<Vec<u8>> {
        let mut replacements = Vec::new();
        let mut appended = Vec::new();
        for (key, value) in changes {
            if !valid_key(key) {
                return Err(problem(None, None, "环境变量名无效"));
            }
            let assignment = self.assignments.get(key);
            if value.contains(['\n', '\r', '\0']) {
                return Err(problem(
                    assignment.map(|a| a.line),
                    Some(key),
                    "值不能包含换行、回车或 NUL",
                ));
            }
            if self.values.get(key) == Some(value) {
                continue;
            }
            let quoted = crate::common::shell_quote(value)
                .map_err(|_| problem(assignment.map(|a| a.line), Some(key), "无法编码字面值"))?;
            match assignment {
                Some(assignment) => replacements.push((assignment.value.clone(), quoted)),
                None => appended.push((key, quoted)),
            }
        }
        replacements.sort_by_key(|(span, _)| span.start);
        let mut result = Vec::new();
        let mut cursor = 0;
        for (span, quoted) in replacements {
            result.extend_from_slice(&self.original[cursor..span.start]);
            result.extend_from_slice(quoted.as_bytes());
            cursor = span.end;
        }
        result.extend_from_slice(&self.original[cursor..]);
        if !appended.is_empty() && !result.is_empty() && !result.ends_with(b"\n") {
            result.extend_from_slice(self.line_ending);
        }
        for (key, quoted) in appended {
            result.extend_from_slice(key.as_bytes());
            result.push(b'=');
            result.extend_from_slice(quoted.as_bytes());
            result.extend_from_slice(self.line_ending);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn literal_values_decode_without_losing_the_original_document() {
        let cases = [
            ("KEY=plain", "plain"),
            ("KEY=some=value", "some=value"),
            ("  export\tKEY='one two' # comment\r\n", "one two"),
            ("KEY=\n", ""),
            ("KEY= # empty\n", ""),
            ("KEY=\"\"\n", ""),
            ("KEY=#fragment\n", "#fragment"),
            ("KEY=plain#fragment # comment\n", "plain#fragment"),
            ("KEY='one'\\''two'\n", "one'two"),
            (
                "KEY='literal $HOME; `id` $(id)'\n",
                "literal $HOME; `id` $(id)",
            ),
            ("KEY=one\\ two\\#three\n", "one two#three"),
            (
                "KEY=\"literal \\$HOME \\`id\\` \\\"quote\\\"\"\n",
                "literal $HOME `id` \"quote\"",
            ),
            ("KEY=\"keeps\\q\"\n", "keeps\\q"),
            ("KEY='中文'\" 和 \"text\n", "中文 和 text"),
        ];
        for (source, expected) in cases {
            let document = EnvDocument::parse(source.as_bytes()).unwrap();
            assert_eq!(
                document.values().get("KEY").map(String::as_str),
                Some(expected)
            );
            assert_eq!(
                document.update(&BTreeMap::new()).unwrap(),
                source.as_bytes()
            );
            assert_eq!(
                document.update(&changes(&[("KEY", expected)])).unwrap(),
                source.as_bytes()
            );
        }
        for value in [
            "",
            "plain",
            "space value",
            "a'b",
            "$HOME; `id` $(id)",
            "#hash",
            "\"quote\"",
            "中文",
            "\\literal",
            "\t",
        ] {
            let source = format!("KEY={}\n", crate::common::shell_quote(value).unwrap());
            let document = EnvDocument::parse(source.as_bytes()).unwrap();
            assert_eq!(document.values()["KEY"], value);
        }
    }

    #[test]
    fn expressions_ambiguous_assignments_and_bad_text_are_rejected_without_values() {
        let cases = [
            "KEY=$private_sentinel",
            "KEY=\"$private_sentinel\"",
            "KEY=$(private_sentinel)",
            "KEY=\"$(private_sentinel)\"",
            "KEY=`private_sentinel`",
            "KEY=value;private_sentinel",
            "KEY=value&&private_sentinel",
            "KEY=value|private_sentinel",
            "KEY=value>private_sentinel",
            "KEY=<(private_sentinel)",
            "KEY=(private_sentinel)",
            "KEY=~private_sentinel",
            "KEY==private_sentinel",
            "KEY=path:=private_sentinel",
            "KEY=*private_sentinel",
            "KEY={private_sentinel,other}",
            "KEY=value OTHER=private_sentinel",
            "KEY= private_sentinel",
            "KEY =private_sentinel",
            "export KEY",
            "source private_sentinel",
            "KEY='private_sentinel",
            "KEY=\"private_sentinel",
            "KEY=private_sentinel\\\nnext",
            "KEY=private_sentinel\r",
            "KEY=private_sentinel\0",
            "KEY=one\nexport KEY=private_sentinel\n",
            "KEY+=private_sentinel",
        ];
        for source in cases {
            let error = EnvDocument::parse(source.as_bytes())
                .err()
                .expect("must reject");
            let text = error.to_string();
            assert!(
                !text.contains("private_sentinel"),
                "error disclosed a value"
            );
            assert!(text.contains("环境文件第 "), "{text}");
        }
        let error = EnvDocument::parse(b"# comment\nKEY=\xff")
            .err()
            .expect("must reject UTF-8");
        assert!(error.to_string().contains("第 2 行"));
        let error = EnvDocument::parse(b"KEY=one\nexport KEY=two")
            .err()
            .expect("must reject duplicate");
        assert_eq!(error.to_string(), "环境文件第 2 行（KEY）：变量重复赋值");
    }

    #[test]
    fn updates_preserve_unrelated_bytes_comments_order_and_crlf() {
        let source = b"# private comment $UNCHANGED\r\n\texport B='old value' \t# keep B\r\nA=\"unchanged\"\r\nHASH=#literal\r\nEMPTY= # keep empty\r\nLAST='literal $HOME; value'";
        let document = EnvDocument::parse(source).unwrap();
        let edited = document
            .update(&changes(&[
                ("B", "new ' value"),
                ("A", "unchanged"),
                ("EMPTY", "filled"),
                ("Z_NEW", "last"),
                ("C_NEW", "first"),
            ]))
            .unwrap();
        assert_eq!(edited, b"# private comment $UNCHANGED\r\n\texport B='new '\\'' value' \t# keep B\r\nA=\"unchanged\"\r\nHASH=#literal\r\nEMPTY='filled' # keep empty\r\nLAST='literal $HOME; value'\r\nC_NEW='first'\r\nZ_NEW='last'\r\n");
        let reparsed = EnvDocument::parse(&edited).unwrap();
        let mut expected = document.values().clone();
        expected.extend(changes(&[
            ("B", "new ' value"),
            ("EMPTY", "filled"),
            ("Z_NEW", "last"),
            ("C_NEW", "first"),
        ]));
        assert_eq!(reparsed.values(), &expected);
        assert_eq!(reparsed.update(&expected).unwrap(), edited);
        assert_eq!(document.update(&BTreeMap::new()).unwrap(), source);
    }

    #[test]
    fn missing_assignments_append_without_changing_existing_line_endings() {
        for (source, expected) in [
            ("", "NEW='value'\n"),
            ("# comment", "# comment\nNEW='value'\n"),
            ("A=one\n", "A=one\nNEW='value'\n"),
            ("A=one\r\n# last", "A=one\r\n# last\r\nNEW='value'\r\n"),
            ("A=one\r\nB=two\n", "A=one\r\nB=two\nNEW='value'\r\n"),
        ] {
            let document = EnvDocument::parse(source.as_bytes()).unwrap();
            assert_eq!(
                document.update(&changes(&[("NEW", "value")])).unwrap(),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn invalid_updates_never_disclose_values_or_modify_the_document() {
        let source = b"KEY='original' # keep\n";
        let document = EnvDocument::parse(source).unwrap();
        for (key, value) in [
            ("KEY", "private_sentinel\nnext"),
            ("NEW", "private_sentinel\0"),
            ("BAD-private_sentinel", "value"),
            ("", "private_sentinel"),
            ("1KEY", "private_sentinel"),
        ] {
            let error = document.update(&changes(&[(key, value)])).unwrap_err();
            assert!(!error.to_string().contains("private_sentinel"));
            assert_eq!(document.update(&BTreeMap::new()).unwrap(), source);
        }
    }
}
