// Minimal LSP server over stdio (Content-Length framed JSON-RPC).
// Features: diagnostics (parse errors), hover (fn signature/variants,
// builtin docs), goto-definition (fn/grow fn defs), completion
// (builtins + in-doc functions + grow variants).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};

const BUILTINS: &[&str] = &[
    "len", "push", "keys", "values", "split", "join", "trim", "upper",
    "lower", "replace", "startswith", "endswith", "contains", "sort",
    "reverse", "range", "map", "filter", "reduce", "enumerate", "zip",
    "flatten", "abs", "min", "max", "floor", "ceil", "sqrt", "pow",
    "sin", "cos", "tan", "log", "exp", "pi", "random", "random_int",
    "type_of", "str", "int", "float", "bool", "dict", "list", "input",
    "print", "eprint", "args", "exit", "env", "read_file",
    "write_file", "append_file", "exists", "read_dir", "spawn",
    "join", "pmap", "fetch", "post", "json_parse", "json_stringify",
    "re_match", "re_find", "re_replace", "re_split", "re_captures",
    "b64_encode", "b64_decode", "url_encode", "url_decode", "sha256",
    "match_path", "call_best", "garden", "format", "read_stdin",
];

struct Doc {
    text: String,
}

pub fn run_lsp() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut docs: HashMap<String, Doc> = HashMap::new();
    let mut buf = Vec::new();
    let mut input = [0u8; 8192];

    loop {
        // Read whatever's available; frame is Content-Length delimited.
        let n = match stdin.lock().read(&mut input) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&input[..n]);

        while let Some((msg, rest)) = take_frame(&buf) {
            buf = rest;
            let req: Value = match serde_json::from_slice(&msg) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let method = req["method"].as_str().unwrap_or("");
            let id = req.get("id").cloned();
            let params = &req["params"];

            let response = match method {
                "initialize" => Some(json!({
                    "capabilities": {
                        "textDocumentSync": 1,
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "documentSymbolProvider": true,
                        "workspaceSymbolProvider": true,
                        "documentHighlightProvider": true,
                        "renameProvider": true,
                        "referencesProvider": true,
                        "signatureHelpProvider": {"triggerCharacters": ["(", ","]},
                        "codeLensProvider": {"resolveProvider": false},
                        "foldingRangeProvider": true,
                        "semanticTokensProvider": {
                            "legend": {"tokenTypes": ["keyword","function","variable","string","number","comment","property","operator"], "tokenModifiers": []},
                            "full": true,
                        },
                        "completionProvider": {"triggerCharacters": [".", ":"]}
                    },
                    "serverInfo": {"name": "veldt-lsp", "version": env!("CARGO_PKG_VERSION")}
                })),
                "shutdown" => Some(Value::Null),
                "exit" => return,
                "textDocument/hover" => hover(params, &docs),
                "textDocument/definition" => definition(params, &docs),
                "textDocument/completion" => completion(params, &docs),
                "textDocument/documentSymbol" => document_symbol(params, &docs),
                "workspace/symbol" => workspace_symbol(params, &docs),
                "textDocument/documentHighlight" => document_highlight(params, &docs),
                "textDocument/rename" => rename(params, &docs),
                "textDocument/references" => references(params, &docs),
                "textDocument/signatureHelp" => signature_help(params, &docs),
                "textDocument/codeLens" => code_lens(params, &docs),
                "textDocument/semanticTokens/full" => semantic_tokens(params, &docs),
                "textDocument/foldingRange" => folding_range(params, &docs),
                _ => None,
            };

            if let Some(result) = response {
                if let Some(id) = id {
                    write_msg(&mut stdout.lock(), &json!({
                        "jsonrpc": "2.0", "id": id, "result": result
                    }));
                }
            }

            match method {
                "textDocument/didOpen" => {
                    let td = &params["textDocument"];
                    let uri = td["uri"].as_str().unwrap_or("").to_string();
                    let text = td["text"].as_str().unwrap_or("").to_string();
                    docs.insert(uri.clone(), Doc { text: text.clone() });
                    publish_diag(&mut stdout.lock(), &uri, &text);
                }
                "textDocument/didChange" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or("").to_string();
                    if let Some(text) = params["contentChanges"][0]["text"].as_str() {
                        docs.insert(uri.clone(), Doc { text: text.to_string() });
                        publish_diag(&mut stdout.lock(), &uri, text);
                    }
                }
                "textDocument/didClose" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                    docs.remove(uri);
                }
                _ => {}
            }
        }
    }
}

fn take_frame(buf: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let s = String::from_utf8_lossy(buf);
    let head_end = s.find("\r\n\r\n")? + 4;
    let len: usize = s[..head_end].lines()
        .find_map(|l| l.strip_prefix("Content-Length:"))
        .and_then(|v| v.trim().parse().ok())?;
    if buf.len() < head_end + len { return None; }
    Some((buf[head_end..head_end + len].to_vec(), buf[head_end + len..].to_vec()))
}

fn write_msg(w: &mut impl Write, v: &Value) {
    let body = serde_json::to_string(v).unwrap();
    let _ = write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = w.flush();
}

/// Parse → one diagnostic per error (or none). Parser errors carry
/// "Line N:" prefixes we surface as the diagnostic range.
fn publish_diag(w: &mut impl Write, uri: &str, text: &str) {
    let mut lx = crate::lexer::Lexer::new(text);
    let err = match lx.tokenize() {
        Err(e) => Some(e.to_string()),
        Ok(tokens) => {
            let mut p = crate::parser::Parser::new_with_lines(tokens, lx.token_lines.clone());
            p.parse_program().err().map(|e| e.to_string())
        }
    };
    let diags = match err {
        Some(e) => {
            let line = e.split("Line ").nth(1)
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1).saturating_sub(1);
            vec![json!({
                "range": {"start": {"line": line, "character": 0},
                          "end": {"line": line, "character": 200}},
                "severity": 1, "source": "veldt", "message": e
            })]
        }
        None => vec![],
    };
    write_msg(w, &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": diags}}));
}

fn word_at(text: &str, line: u32, ch: u32) -> String {
    let l = text.lines().nth(line as usize).unwrap_or("");
    let bytes = l.as_bytes();
    let c = ch as usize;
    if c > bytes.len() { return String::new(); }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = c.min(bytes.len().saturating_sub(1));
    while start > 0 && is_word(bytes[start - 1]) { start -= 1; }
    let mut end = c;
    while end < bytes.len() && is_word(bytes[end]) { end += 1; }
    String::from_utf8_lossy(&bytes[start..end.max(start)]).to_string()
}

fn doc_of<'a>(params: &Value, docs: &'a HashMap<String, Doc>) -> Option<&'a Doc> {
    docs.get(params["textDocument"]["uri"].as_str()?)
}

/// `fn name(` / `grow fn name(` definitions in the doc → line numbers.
fn find_defs(text: &str, name: &str) -> Vec<u32> {
    text.lines().enumerate().filter(|(_, l)| {
        let t = l.trim_start();
        (t.starts_with("fn ") || t.starts_with("grow fn "))
            && t.trim_start_matches("grow ").trim_start_matches("fn ")
                .starts_with(&format!("{}(", name))
    }).map(|(i, _)| i as u32).collect()
}

/// Outline view — top-level grow fn/fn/struct declarations + lets.
/// LSP SymbolKind: 12 Function, 13 Variable, 23 Struct.
fn document_symbol(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let mut out = Vec::new();
    for (line, text) in doc.text.lines().enumerate() {
        let t = text.trim_start();
        let (kind, name) = if t.starts_with("grow fn ") || t.starts_with("fn ") {
            let n = t.trim_start_matches("grow fn ").trim_start_matches("fn ")
                .split(|c: char| c == '(' || c == '<' || c.is_whitespace())
                .next().unwrap_or("").to_string();
            (12u32, n)
        } else if t.starts_with("struct ") {
            (23u32, t[7..].split(|c: char| c == '{' || c.is_whitespace())
                .next().unwrap_or("").to_string())
        } else if t.starts_with("let ") {
            (13u32, t[4..].split(|c: char| c == '=' || c == ':' || c.is_whitespace())
                .next().unwrap_or("").to_string())
        } else { continue };
        if name.is_empty() { continue; }
        out.push(json!({
            "name": name, "kind": kind,
            "range": {"start": {"line": line as u32, "character": 0},
                      "end": {"line": line as u32, "character": text.len() as u32}},
            "selectionRange": {"start": {"line": line as u32, "character": 0},
                               "end": {"line": line as u32, "character": text.len() as u32}},
        }));
    }
    Some(Value::Array(out))
}

/// Project-wide symbol search — scans every open doc for decls matching
/// the query; SymbolInformation uses the legacy LSP shape most clients
/// still accept (kind/location/containerName).
fn workspace_symbol(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let q = params["query"].as_str().unwrap_or("").to_lowercase();
    let mut out = Vec::new();
    for (uri, doc) in docs {
        for (line, text) in doc.text.lines().enumerate() {
            let t = text.trim_start();
            let (kind, name) = if t.starts_with("grow fn ") || t.starts_with("fn ") {
                (12u32, t.trim_start_matches("grow fn ").trim_start_matches("fn ")
                    .split(|c: char| c == '(' || c == '<' || c.is_whitespace()).next().unwrap_or(""))
            } else if t.starts_with("struct ") {
                (23u32, t[7..].split(|c: char| c == '{' || c.is_whitespace()).next().unwrap_or(""))
            } else if t.starts_with("let ") {
                (13u32, t[4..].split(|c: char| c == '=' || c == ':' || c.is_whitespace()).next().unwrap_or(""))
            } else { continue };
            if name.is_empty() || (!q.is_empty() && !name.to_lowercase().contains(&q)) { continue; }
            out.push(json!({
                "name": name, "kind": kind,
                "location": {"uri": uri,
                    "range": {"start": {"line": line as u32, "character": 0},
                              "end": {"line": line as u32, "character": text.len() as u32}}},
                "containerName": uri.split('/').last().unwrap_or(uri),
            }));
        }
    }
    Some(Value::Array(out))
}

/// Highlight every occurrence of the word under the cursor —
/// DocumentHighlight kind 1 = text. Cheap whole-doc word scan.
fn document_highlight(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let line = params["position"]["line"].as_u64()? as usize;
    let ch = params["position"]["character"].as_u64()? as usize;
    let word = word_at(&doc.text, line as u32, ch as u32);
    if word.is_empty() { return Some(json!([])); }
    let mut out = Vec::new();
    for (l, line_text) in doc.text.lines().enumerate() {
        let mut start = 0;
        while let Some(pos) = line_text[start..].find(word.as_str()) {
            let col = start + pos;
            // word-boundary check — don't match inside a longer identifier
            let before = line_text[..col].chars().last().unwrap_or(' ');
            let after = line_text[col + word.len()..].chars().next().unwrap_or(' ');
            if !before.is_alphanumeric() && before != '_' && !after.is_alphanumeric() && after != '_' {
                out.push(json!({
                    "range": {"start": {"line": l as u32, "character": col as u32},
                              "end": {"line": l as u32, "character": (col + word.len()) as u32}},
                    "kind": 1,
                }));
            }
            start = col + word.len();
        }
    }
    Some(Value::Array(out))
}

/// Rename a symbol — returns WorkspaceEdit with TextEdits for every
/// boundary-exact occurrence of the word under the cursor.
fn rename(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let uri = params["textDocument"]["uri"].as_str()?;
    let line = params["position"]["line"].as_u64()? as usize;
    let ch = params["position"]["character"].as_u64()? as usize;
    let new_name = params["newName"].as_str()?;
    let word = word_at(&doc.text, line as u32, ch as u32);
    if word.is_empty() { return Some(json!({})); }
    let mut edits = Vec::new();
    for (l, line_text) in doc.text.lines().enumerate() {
        let mut start = 0;
        while let Some(pos) = line_text[start..].find(word.as_str()) {
            let col = start + pos;
            let before = line_text[..col].chars().last().unwrap_or(' ');
            let after = line_text[col + word.len()..].chars().next().unwrap_or(' ');
            if !before.is_alphanumeric() && before != '_' && !after.is_alphanumeric() && after != '_' {
                edits.push(json!({
                    "range": {"start": {"line": l as u32, "character": col as u32},
                              "end": {"line": l as u32, "character": (col + word.len()) as u32}},
                    "newText": new_name,
                }));
            }
            start = col + word.len();
        }
    }
    Some(json!({ "changes": { uri: edits } }))
}

/// Find every occurrence of the word under the cursor across all open
/// docs — Location[] {uri, range} in LSP shape.
fn references(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let line = params["position"]["line"].as_u64()? as usize;
    let ch = params["position"]["character"].as_u64()? as usize;
    let word = word_at(&doc.text, line as u32, ch as u32);
    if word.is_empty() { return Some(json!([])); }
    let mut out = Vec::new();
    for (uri, d) in docs {
        for (l, line_text) in d.text.lines().enumerate() {
            let mut start = 0;
            while let Some(pos) = line_text[start..].find(word.as_str()) {
                let col = start + pos;
                let before = line_text[..col].chars().last().unwrap_or(' ');
                let after = line_text[col + word.len()..].chars().next().unwrap_or(' ');
                if !before.is_alphanumeric() && before != '_' && !after.is_alphanumeric() && after != '_' {
                    out.push(json!({
                        "uri": uri,
                        "range": {"start": {"line": l as u32, "character": col as u32},
                                  "end": {"line": l as u32, "character": (col + word.len()) as u32}},
                    }));
                }
                start = col + word.len();
            }
        }
    }
    Some(Value::Array(out))
}

/// Signature help on ( — resolves the enclosing call's name, looks up
/// the fn's decl in the open doc (fn/grow fn) or builtin signature table.
fn signature_help(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let line = params["position"]["line"].as_u64()? as usize;
    let ch = params["position"]["character"].as_u64()? as usize;
    // scan back to the enclosing '(' — the fn name is the word before it
    let text = doc.text.lines().nth(line)?;
    let before = &text[..ch.min(text.len())];
    let mut depth = 0i32;
    let mut fn_start = None;
    for (i, c) in before.char_indices().rev() {
        match c {
            ')' | ']' => depth += 1,
            '(' if depth > 0 => depth -= 1,
            '(' => { fn_start = Some(i); break; }
            _ => {}
        }
    }
    let open = fn_start?;
    let name = before[..open].chars().rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect::<String>().chars().rev().collect::<String>();
    if name.is_empty() { return Some(json!({})); }
    // signature: find `fn name(` or `grow fn name(` in the doc, take param list
    let sig = doc.text.lines().find_map(|l| {
        let t = l.trim_start();
        if (t.starts_with("fn ") || t.starts_with("grow fn ")) {
            let rest = t.trim_start_matches("grow fn ").trim_start_matches("fn ");
            if rest.starts_with(&format!("{}(", name)) {
                return Some(rest.split(')').next().unwrap_or(rest).to_string() + ")");
            }
        }
        None
    }).unwrap_or_else(|| format!("{}(...)", name));
    Some(json!({
        "signatures": [{"label": sig}],
        "activeSignature": 0,
        "activeParameter": before[open+1..].matches(',').count() as u32,
    }))
}

/// Code lens — "N tests · fitness F" above every `grow fn` in the doc.
/// Loads the garden once per request (cheap, single JSON read).
fn code_lens(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let mut veldt = crate::veldt::Veldt::new();
    if veldt.load().is_err() { return Some(json!([])); }
    let mut out = Vec::new();
    for (line, text) in doc.text.lines().enumerate() {
        let t = text.trim_start();
        if !t.starts_with("grow fn ") && !t.starts_with("fn ") { continue; }
        let name = t.trim_start_matches("grow fn ").trim_start_matches("fn ")
            .split(|c: char| c == '(' || c == '<' || c.is_whitespace())
            .next().unwrap_or("");
        if name.is_empty() { continue; }
        let title = if let Some(vs) = veldt.functions.get(name) {
            let top = vs.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);
            let tests = veldt.probes.get(name).map(|p| p.len()).unwrap_or(0);
            format!("{} tests · fitness {:.2} · {} variants", tests, top, vs.len())
        } else {
            "no garden lineage yet".to_string()
        };
        out.push(json!({
            "range": {"start": {"line": line as u32, "character": 0},
                      "end": {"line": line as u32, "character": text.len() as u32}},
            "command": {"title": title, "command": ""},
        }));
    }
    Some(Value::Array(out))
}

/// Full-document semantic tokens — a lightweight line scan that maps
/// keywords/fn names/strings/numbers/comments to the declared legend.
/// delta-encoded (line, startChar, len, tokenType, modifiers).
fn semantic_tokens(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let kw = ["let","fn","grow","if","else","while","for","return","print","test",
        "try","catch","struct","match","in","break","continue","import","as","null","true","false"];
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut data: Vec<u32> = Vec::new();
    let mut prev = (0usize, 0usize);
    for (line, text) in doc.text.lines().enumerate() {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c == '"' { // string literal
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' { i += 1; }
                emit(&mut data, line, start, (i - start + 1).min(text.len() - start), 3, &mut prev);
            } else if c.is_digit(10) && (i == 0 || !is_word(bytes[i-1])) { // number
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') { i += 1; }
                emit(&mut data, line, start, i - start, 4, &mut prev);
            } else if c.is_alphabetic() || c == '_' { // identifier-ish
                let start = i;
                while i < bytes.len() && is_word(bytes[i]) { i += 1; }
                let w = &text[start..i];
                let kind = if kw.contains(&w) { 0 }
                    else if i < bytes.len() && bytes[i] == b'(' { 1 }
                    else { 2 };
                emit(&mut data, line, start, i - start, kind, &mut prev);
            } else if text[i..].starts_with("//") { // comment — rest of line
                emit(&mut data, line, i, text.len() - i, 5, &mut prev); break;
            } else if "+-*/%<>!=&|?".contains(c) {
                emit(&mut data, line, i, 1, 7, &mut prev);
            }
            i += 1;
        }
    }
    Some(json!({"data": data}))
}

fn emit(data: &mut Vec<u32>, line: usize, col: usize, len: usize, tt: u32, prev: &mut (usize, usize)) {
    // LSP spec: tokens are delta-encoded — (deltaLine, deltaStartChar,
    // len, type, mods) relative to the previous token.
    let dl = line.saturating_sub(prev.0);
    let dc = if dl == 0 { col.saturating_sub(prev.1) } else { col };
    data.push(dl as u32); data.push(dc as u32); data.push(len as u32);
    data.push(tt); data.push(0);
    *prev = (line, col);
}

/// Foldable regions — brace-delimited blocks (fn/if/while/for/try/
/// match/struct/grow fn) get startLine..endLine ranges.
fn folding_range(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let openers = ["fn ","grow fn ","if ","while ","for ","try ","match ","struct "];
    let mut out = Vec::new();
    let mut stack: Vec<(usize, u32)> = Vec::new(); // (line, depth)
    let mut depth = 0u32;
    for (line, text) in doc.text.lines().enumerate() {
        let t = text.trim_start();
        let is_opener = openers.iter().any(|o| t.starts_with(o)) && t.contains('{');
        if is_opener {
            stack.push((line, depth + text.matches('{').count() as u32
                - text.matches('}').count() as u32));
        }
        depth = (depth as i64 + text.matches('{').count() as i64
            - text.matches('}').count() as i64).max(0) as u32;
        while let Some(&(start_line, start_depth)) = stack.last() {
            if depth < start_depth && line > start_line {
                out.push(json!({"startLine": start_line as u32, "endLine": line as u32}));
                stack.pop();
            } else { break; }
        }
    }
    Some(Value::Array(out))
}

fn hover(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let pos = &params["position"];
    let w = word_at(&doc.text, pos["line"].as_u64()? as u32, pos["character"].as_u64()? as u32);
    if w.is_empty() { return Some(Value::Null); }
    let content = if BUILTINS.contains(&w.as_str()) {
        format!("builtin `{}` — see `veldt docs {}`", w, w)
    } else {
        let defs = find_defs(&doc.text, &w);
        if defs.is_empty() { return Some(Value::Null); }
        let sig = doc.text.lines().nth(defs[0] as usize).unwrap_or("").trim();
        format!("`{}` — defined at line {}:\n```veldt\n{}\n```", w, defs[0] + 1, sig)
    };
    Some(json!({"contents": {"kind": "markdown", "value": content}}))
}

fn definition(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let doc = doc_of(params, docs)?;
    let pos = &params["position"];
    let w = word_at(&doc.text, pos["line"].as_u64()? as u32, pos["character"].as_u64()? as u32);
    let uri = params["textDocument"]["uri"].as_str()?;
    let locs: Vec<Value> = find_defs(&doc.text, &w).iter().map(|&l| json!({
        "uri": uri, "range": {"start": {"line": l, "character": 0},
                              "end": {"line": l, "character": 60}}
    })).collect();
    Some(if locs.len() == 1 { locs[0].clone() } else { json!(locs) })
}

fn completion(params: &Value, docs: &HashMap<String, Doc>) -> Option<Value> {
    let mut items: Vec<Value> = BUILTINS.iter().map(|b| json!({
        "label": b, "kind": 3 // Function
    })).collect();
    if let Some(doc) = doc_of(params, docs) {
        for kw in ["fn", "grow", "let", "while", "for", "if", "else",
                   "return", "match", "test", "try", "spawn", "import"] {
            items.push(json!({"label": kw, "kind": 14})); // Keyword
        }
        // in-doc function names
        for (i, l) in doc.text.lines().enumerate() {
            let t = l.trim_start();
            for pre in ["fn ", "grow fn "] {
                if t.starts_with(pre) {
                    if let Some(name) = t[pre.len()..].split('(').next() {
                        items.push(json!({
                            "label": name.trim(), "kind": 3,
                            "detail": format!("defined line {}", i + 1)
                        }));
                    }
                }
            }
        }
    }
    Some(json!({"isIncomplete": false, "items": items}))
}
