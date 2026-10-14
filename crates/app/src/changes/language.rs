//! File path → gpui-component highlighter language, limited to the grammars enabled in
//! `Cargo.toml`.

use std::path::Path;

/// Language name understood by `gpui_kit::component::highlighter::LanguageRegistry`, or
/// `None` for plain text.
pub fn language_for_path(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    let by_name = match name {
        ".bashrc" | ".bash_profile" | ".zshrc" | ".zprofile" | ".profile" | ".envrc" => {
            Some("bash")
        }
        "Cargo.lock" | "uv.lock" | "poetry.lock" => Some("toml"),
        ".babelrc" | ".eslintrc" | ".prettierrc" | "composer.lock" => Some("json"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "java" => "java",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "rs" => "rust",
        "sh" | "bash" | "zsh" | "command" => "bash",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" | "mdx" => "markdown",
        "css" | "scss" => "css",
        "html" | "htm" | "xhtml" => "html",
        "json" | "jsonc" | "json5" | "geojson" | "xcstrings" => "json",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::language_for_path;
    use std::path::Path;

    #[test]
    fn detects_by_extension_and_name() {
        let cases = [
            ("ios/App/ContentView.swift", Some("swift")),
            ("android/app/build.gradle.kts", Some("kotlin")),
            ("Main.KT", Some("kotlin")),
            ("src/App.java", Some("java")),
            ("src/index.ts", Some("typescript")),
            ("src/App.tsx", Some("tsx")),
            ("src/App.jsx", Some("javascript")),
            ("metro.config.cjs", Some("javascript")),
            ("crates/app/src/main.rs", Some("rust")),
            ("scripts/build.sh", Some("bash")),
            (".zshrc", Some("bash")),
            (".github/workflows/ci.yml", Some("yaml")),
            ("Cargo.toml", Some("toml")),
            ("Cargo.lock", Some("toml")),
            ("README.md", Some("markdown")),
            ("styles/app.scss", Some("css")),
            ("public/index.html", Some("html")),
            ("package.json", Some("json")),
            ("Localizable.xcstrings", Some("json")),
            ("lib/main.dart", None),
            ("Makefile", None),
            ("image.png", None),
            ("", None),
        ];
        for (path, expected) in cases {
            assert_eq!(language_for_path(Path::new(path)), expected, "{path}");
        }
    }
}
