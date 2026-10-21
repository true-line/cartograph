//! Vue single-file components (#595, AC-0268): the top-level `<script>` and
//! `<script setup>` blocks parse with this crate's JavaScript/TypeScript
//! extractor over [`adapters_fw::vue::script_view`], whose same-length view
//! keeps every span, line and column in the `.vue` file's own coordinates.
//! Templates, styles and custom blocks are not parsed.

use std::borrow::Cow;

/// Component extensions collected beside [`crate::SOURCE_EXTENSIONS`]. Kept
/// apart so the extensionless-import guess never invents a `.vue` target.
pub use adapters_fw::vue::COMPONENT_EXTENSIONS;

/// The bytes the parser sees for `path`: a `.vue` component's script view,
/// any other source unchanged.
pub(crate) fn parse_view<'a>(source: &'a [u8], path: &str) -> Cow<'a, [u8]> {
    if adapters_fw::vue::is_component(path) {
        Cow::Owned(adapters_fw::vue::script_view(source))
    } else {
        Cow::Borrowed(source)
    }
}

/// For a `.vue` component, whether its script takes the JSX-aware grammar:
/// plain TypeScript (`lang="ts"`) keeps the TypeScript grammar for its
/// angle-bracket casts, every other dialect takes TSX. `None` otherwise.
pub(crate) fn component_tsx(source: &[u8], path: &str) -> Option<bool> {
    adapters_fw::vue::is_component(path).then(|| !adapters_fw::vue::plain_typescript(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_components_get_a_script_view() {
        let plain = b"const a = '<script>'";
        assert!(matches!(parse_view(plain, "a.ts"), Cow::Borrowed(_)));
        assert!(matches!(parse_view(plain, "A.vue"), Cow::Owned(_)));
    }
}
