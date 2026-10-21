//! Vue single-file component layout (#595, AC-0268): which bytes of a `.vue`
//! file are script. Shared by the TypeScript extractor and Preflight's
//! risky-construct scan so both read exactly the same script text.
//!
//! [`script_view`] keeps the contents of the top-level `<script>` and
//! `<script setup>` blocks and blanks every other byte to a space (line
//! breaks kept), so offsets, lines and columns still name the same place in
//! the component. Templates, styles, custom blocks, comments and blocks in a
//! non-JS `lang` are never script.

/// Component file extensions.
pub const COMPONENT_EXTENSIONS: [&str; 1] = [".vue"];

/// Script languages a block may declare; anything else (`lang="coffee"`)
/// is not JavaScript/TypeScript and stays blank.
const SCRIPT_LANGS: &[&str] = &["js", "jsx", "ts", "tsx"];

/// Whether `path` names a Vue component.
pub fn is_component(path: &str) -> bool {
    COMPONENT_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
}

/// Written over the start of each block's blanked closing tag: U+2028 LINE
/// SEPARATOR, a JavaScript line terminator that is not a line break for row
/// counting, ends a trailing `//` comment, and the `;` ends the statement
/// (the grammars insert semicolons only at `\n`). One block can never run
/// into the next, and every offset, line and column is unchanged.
const BLOCK_BOUNDARY: &[u8] = "\u{2028};".as_bytes();

/// Same length as `source`; only script block contents survive, each
/// followed by a lexical boundary inside its blanked closing tag.
pub fn script_view(source: &[u8]) -> Vec<u8> {
    let mut view: Vec<u8> = source
        .iter()
        .map(|&b| if b == b'\n' || b == b'\r' { b } else { b' ' })
        .collect();
    for block in script_blocks(source) {
        view[block.start..block.end].copy_from_slice(&source[block.start..block.end]);
        // `</script` is at least eight blanked bytes long.
        view[block.end..block.end + BLOCK_BOUNDARY.len()].copy_from_slice(BLOCK_BOUNDARY);
    }
    view
}

/// Whether the component's script is plain TypeScript (`lang="ts"`), whose
/// angle-bracket casts need the non-JSX grammar. One component takes one
/// grammar: Vue compiles `<script>` and `<script setup>` into one module and
/// rejects blocks that declare different languages, so for such an invalid
/// component any `lang="ts"` block selects the TypeScript grammar for all.
pub fn plain_typescript(source: &[u8]) -> bool {
    script_blocks(source)
        .iter()
        .any(|block| block.lang.as_deref() == Some("ts"))
}

/// One top-level script block's content range and declared language.
struct Block {
    start: usize,
    end: usize,
    lang: Option<String>,
}

/// The top-level script blocks, in file order.
fn script_blocks(source: &[u8]) -> Vec<Block> {
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
        if name == b"script" {
            let lang = declared_lang(&source[lt + 1 + name.len()..gt]);
            if lang
                .as_deref()
                .is_none_or(|lang| SCRIPT_LANGS.contains(&lang))
            {
                blocks.push(Block {
                    start: gt + 1,
                    end: close,
                    lang,
                });
            }
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
    let rest = source.get(from..).unwrap_or_default();
    let len = rest
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'-')
        .count();
    &rest[..len]
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

/// Whether `{name}` at `at` names exactly that tag.
fn names_tag(source: &[u8], at: usize, name: &[u8]) -> bool {
    source
        .get(at..at + name.len())
        .is_some_and(|got| got.eq_ignore_ascii_case(name))
        && source
            .get(at + name.len())
            .is_none_or(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
}

/// Where the block `name` whose content starts at `from` closes: the
/// content end and the offset after its closing tag.
fn block_close(source: &[u8], name: &[u8], from: usize) -> Option<(usize, usize)> {
    if name != b"template" {
        // Script, style and custom-block content is raw text: the first
        // closing tag of the same name ends it.
        let mut at = from;
        loop {
            let lt = find(source, at, b"</")?;
            if names_tag(source, lt + 2, name) {
                return Some((lt, tag_end(source, lt)? + 1));
            }
            at = lt + 2;
        }
    }
    // A template is markup: comments and whole tags (quoted attribute values
    // included) are skipped, and nested `<template>` elements are counted.
    let mut depth = 0usize;
    let mut at = from;
    while let Some(lt) = find(source, at, b"<") {
        if source[lt..].starts_with(b"<!--") {
            at = find(source, lt + 4, b"-->")? + 3;
        } else if source.get(lt + 1) == Some(&b'/') {
            let gt = tag_end(source, lt)?;
            if names_tag(source, lt + 2, name) {
                if depth == 0 {
                    return Some((lt, gt + 1));
                }
                depth -= 1;
            }
            at = gt + 1;
        } else if !tag_name(source, lt + 1).is_empty() {
            let gt = tag_end(source, lt)?;
            if names_tag(source, lt + 1, name) && source[gt - 1] != b'/' {
                depth += 1;
            }
            at = gt + 1;
        } else {
            at = lt + 1;
        }
    }
    None
}

/// The attributes of an opening tag (the bytes after its name), as
/// lower-cased names with their unquoted values.
fn attributes(tag: &[u8]) -> Vec<(String, Option<String>)> {
    let text = String::from_utf8_lossy(tag);
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let skip = |i: &mut usize, pred: &dyn Fn(u8) -> bool| {
        while *i < bytes.len() && pred(bytes[*i]) {
            *i += 1;
        }
    };
    loop {
        skip(&mut i, &|b| b.is_ascii_whitespace() || b == b'/');
        if i >= bytes.len() {
            return out;
        }
        let start = i;
        skip(&mut i, &|b| {
            !(b.is_ascii_whitespace() || b == b'=' || b == b'/')
        });
        let name = text[start..i].to_ascii_lowercase();
        skip(&mut i, &|b| b.is_ascii_whitespace());
        let mut value = None;
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            skip(&mut i, &|b| b.is_ascii_whitespace());
            let start = i;
            match bytes.get(i) {
                Some(&q @ (b'"' | b'\'')) => {
                    i += 1;
                    let begin = i;
                    skip(&mut i, &|b| b != q);
                    value = Some(text[begin..i].to_string());
                    i += 1;
                }
                _ => {
                    skip(&mut i, &|b| !b.is_ascii_whitespace());
                    value = Some(text[start..i].to_string());
                }
            }
        }
        if name.is_empty() {
            i += 1;
        } else {
            out.push((name, value));
        }
    }
}

/// The lower-cased `lang` an opening `<script …>` tag's attributes declare,
/// if any. The first `lang` attribute decides, as in HTML.
fn declared_lang(tag: &[u8]) -> Option<String> {
    attributes(tag)
        .into_iter()
        .find(|(name, _)| name == "lang")
        .and_then(|(_, value)| value)
        .map(|lang| lang.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view with block boundaries shown as spaces, for text comparisons.
    fn view(source: &str) -> String {
        String::from_utf8(script_view(source.as_bytes()))
            .unwrap()
            .replace("\u{2028};", "    ")
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
    fn template_comments_and_quoted_markup_never_move_the_template_close() {
        // A commented-out nested template must not swallow later scripts…
        let open = "<template><div><!-- <template v-if=\"old\"> --></div></template>\n\
                    <script>kept()</script>\n";
        assert_eq!(view(open).trim(), "kept()");
        // …and a commented or quoted close must not end the template early.
        let close = "<template><!-- </template> --><p title=\"</template>\"></p>\n\
                     <script>still_template()</script></template>\n\
                     <script>real()</script>\n";
        assert_eq!(view(close).trim(), "real()");
    }

    #[test]
    fn the_lang_attribute_is_parsed_not_searched() {
        for (tag, kept) in [
            ("<script data-note=\"use lang=coffee here\">", true),
            ("<script title=\"lang=js\" lang=\"coffee\">", false),
            ("<script setup LANG=\"TS\">", true),
            ("<script lang=ts>", true),
            ("<script lang = 'coffee'>", false),
            ("<script language=\"coffee\">", true),
        ] {
            let source = format!("{tag}code()</script>\n");
            assert_eq!(view(&source).trim() == "code()", kept, "{tag}");
        }
    }

    #[test]
    fn each_block_ends_lexically_where_it_ends_in_the_component() {
        let source = "<script>const a = 1 // normal</script><script setup>save()</script>\n";
        let raw = script_view(source.as_bytes());
        assert_eq!(raw.len(), source.len());
        let text = String::from_utf8(raw).unwrap();
        let comment_end = text.find("// normal").unwrap() + "// normal".len();
        assert_eq!(&text[comment_end..comment_end + 4], "\u{2028};");
        assert_eq!(text.find("save()"), source.find("save()"));
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn plain_typescript_is_told_apart_from_other_dialects() {
        assert!(plain_typescript(b"<script setup lang=\"ts\">x</script>"));
        // Mixed dialects are invalid Vue; any plain-TS block decides.
        assert!(plain_typescript(
            b"<script lang=\"tsx\">x</script><script setup lang=\"ts\">y</script>"
        ));
        for other in ["<script>x</script>", "<script lang=\"tsx\">x</script>"] {
            assert!(!plain_typescript(other.as_bytes()), "{other}");
        }
    }

    #[test]
    fn unterminated_and_self_closing_blocks_contribute_nothing() {
        assert_eq!(view("<script>open()").trim(), "");
        assert_eq!(view("<script src=\"./a.js\"/>").trim(), "");
        assert_eq!(view("<template><p>").trim(), "");
        assert!(is_component("a/B.vue") && !is_component("a/b.ts"));
    }
}
