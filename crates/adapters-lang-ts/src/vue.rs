//! Vue single-file components (#595, AC-0268): the top-level `<script>` and
//! `<script setup>` blocks parse with this crate's JavaScript/TypeScript
//! extractor. Every byte outside those blocks is blanked to a space (line
//! breaks kept), so the parser sees only script content while every byte
//! offset, line and column still names the same place in the `.vue` file.
//! Templates, styles and custom blocks are not parsed.

use std::borrow::Cow;

/// Component extensions collected beside [`crate::SOURCE_EXTENSIONS`]. Kept
/// apart so the extensionless-import guess never invents a `.vue` target.
pub const COMPONENT_EXTENSIONS: [&str; 1] = [".vue"];

/// Script languages a block may declare; anything else (`lang="coffee"`)
/// is not this grammar's to parse and stays blank.
const SCRIPT_LANGS: &[&str] = &["js", "jsx", "ts", "tsx"];

/// The bytes the parser sees for `path`: a `.vue` component's script view,
/// any other source unchanged.
pub(crate) fn parse_view<'a>(source: &'a [u8], path: &str) -> Cow<'a, [u8]> {
    if COMPONENT_EXTENSIONS.iter().any(|ext| path.ends_with(ext)) {
        Cow::Owned(script_view(source))
    } else {
        Cow::Borrowed(source)
    }
}

/// Same length as `source`; only script block contents survive.
fn script_view(source: &[u8]) -> Vec<u8> {
    let mut view: Vec<u8> = source
        .iter()
        .map(|&b| if b == b'\n' || b == b'\r' { b } else { b' ' })
        .collect();
    for (start, end) in script_blocks(source) {
        view[start..end].copy_from_slice(&source[start..end]);
    }
    view
}

/// Content ranges of the top-level script blocks, in file order.
fn script_blocks(source: &[u8]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut at = 0;
    while let Some(lt) = find(source, at, b"<") {
        if source[lt..].starts_with(b"<!--") {
            at = find(source, lt + 4, b"-->").map_or(source.len(), |end| end + 3);
            continue;
        }
        let name = tag_name(source, lt + 1);
        let Some(gt) = (!name.is_empty()).then(|| tag_end(source, lt)).flatten() else {
            at = lt + 1;
            continue;
        };
        if source[gt - 1] == b'/' {
            at = gt + 1;
            continue;
        }
        let name = name.to_ascii_lowercase();
        let Some((close, after)) = block_close(source, &name, gt + 1) else {
            break;
        };
        if name == b"script" && script_lang(&source[lt..gt]) {
            blocks.push((gt + 1, close));
        }
        at = after;
    }
    blocks
}

fn find(source: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    source
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn tag_name(source: &[u8], from: usize) -> &[u8] {
    let len = source[from..]
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'-')
        .count();
    &source[from..from + len]
}

/// The `>` closing the tag opened at `lt`, skipping quoted attribute values.
fn tag_end(source: &[u8], lt: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, &b) in source[lt..].iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(lt + offset),
            None => {}
        }
    }
    None
}

/// Whether `<{name}` or `</{name}` at `at` names exactly that tag.
fn names_tag(source: &[u8], at: usize, name: &[u8]) -> bool {
    source
        .get(at..at + name.len())
        .is_some_and(|got| got.eq_ignore_ascii_case(name))
        && source
            .get(at + name.len())
            .is_none_or(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
}

/// Where the block `name` whose content starts at `from` closes: the
/// content end and the offset after its closing tag. Script and style
/// content is raw text, so their first closing tag ends them; a template
/// nests `<template>` elements, so its closing tags are counted.
fn block_close(source: &[u8], name: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    let mut at = from;
    while let Some(lt) = find(source, at, b"<") {
        if source.get(lt + 1) == Some(&b'/') && names_tag(source, lt + 2, name) {
            let gt = tag_end(source, lt)?;
            if depth == 0 {
                return Some((lt, gt + 1));
            }
            depth -= 1;
            at = gt + 1;
        } else if name == b"template" && names_tag(source, lt + 1, name) {
            let gt = tag_end(source, lt)?;
            if source[gt - 1] != b'/' {
                depth += 1;
            }
            at = gt + 1;
        } else {
            at = lt + 1;
        }
    }
    None
}

/// Whether an opening `<script …>` tag declares a JS/TS dialect (or none).
fn script_lang(tag: &[u8]) -> bool {
    let tag = String::from_utf8_lossy(tag);
    let mut rest = tag.as_ref();
    while let Some(at) = rest.find("lang") {
        let before = rest[..at].chars().next_back();
        let after = rest[at + 4..].trim_start();
        rest = &rest[at + 4..];
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let Some(value) = after.strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let lang = match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or(""),
        };
        return SCRIPT_LANGS.contains(&lang.to_ascii_lowercase().as_str());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(source: &str) -> String {
        String::from_utf8(script_view(source.as_bytes())).unwrap()
    }

    #[test]
    fn only_script_and_script_setup_content_survives_at_its_offsets() {
        let source = "<template>\n  <div @click=\"go()\">{{ a }}</div>\n</template>\n\
                      <script>\nexport default { name: 'A' }\n</script>\n\
                      <script setup lang=\"ts\">\nconst a: number = 1\n</script>\n\
                      <style scoped>\n.a { color: red }\n</style>\n";
        let got = view(source);
        assert_eq!(got.len(), source.len());
        assert_eq!(got.lines().count(), source.lines().count());
        for kept in ["export default { name: 'A' }", "const a: number = 1"] {
            assert_eq!(got.find(kept), source.find(kept), "{kept} moved");
        }
        for blanked in ["template", "go()", "script", "color", "style"] {
            assert!(!got.contains(blanked), "{blanked} survived");
        }
    }

    #[test]
    fn nested_templates_comments_and_foreign_langs_stay_blank() {
        let source = "<!-- <script>bad()</script> -->\n\
                      <template><template v-if=\"x\"><script>no()</script></template></template>\n\
                      <script lang=\"coffee\">coffee()</script>\n\
                      <i18n>{\"a\": \"<script>\"}</i18n>\n\
                      <script lang='tsx'>yes()</script>\n";
        let got = view(source);
        assert_eq!(got.trim(), "yes()");
        assert_eq!(got.find("yes()"), source.find("yes()"));
    }

    #[test]
    fn unterminated_and_non_vue_sources_are_handled() {
        assert_eq!(view("<script>open()").trim(), "");
        assert_eq!(view("<script src=\"./a.js\"/>").trim(), "");
        let plain = b"const a = '<script>'";
        assert!(matches!(parse_view(plain, "a.ts"), Cow::Borrowed(_)));
    }
}
