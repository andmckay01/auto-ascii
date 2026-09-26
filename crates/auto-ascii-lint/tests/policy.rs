//! Language fixtures and front matter policy regressions.

use auto_ascii_lint::{Allowlist, Kind, Language, in_scope, language, scan, violation};
use std::collections::BTreeSet;

fn comments(text: &str, lang: Language) -> Vec<(Kind, String)> {
    scan(text, lang)
        .unwrap_or_else(|e| panic!("{lang:?}: {e:#}\n{text}"))
        .into_iter()
        .map(|c| (c.kind, text[c.start..c.end].to_owned()))
        .collect()
}

fn failures(text: &str, lang: Language) -> Vec<String> {
    scan(text, lang)
        .unwrap_or_else(|e| panic!("{lang:?}: {e:#}\n{text}"))
        .iter()
        .filter_map(|c| violation(text, c, lang))
        .collect()
}

#[test]
fn headers_are_optional_and_precede_code_in_every_language() {
    for (lang, marker, code) in [
        (Language::Rust, "//!", "use std::fmt;"),
        (Language::Shell, "#", "echo hello"),
        (Language::Python, "#", "x = 1"),
        (Language::Toml, "#", "x = 1"),
        (Language::Make, "#", "X = 1"),
        (Language::Gitignore, "#", "target/"),
    ] {
        assert!(failures("", lang).is_empty());
        assert!(failures(code, lang).is_empty());
        assert!(failures(&format!("{marker} Owns input.\n{code}\n"), lang).is_empty());
        assert_eq!(
            failures(
                &format!("{marker} Owns input.\n\n{marker} second\n{code}\n"),
                lang
            )
            .len(),
            1
        );
        assert_eq!(failures(&format!("{code}\n{marker} late\n"), lang).len(), 1);
        assert_eq!(
            failures(
                &format!("{marker} one\n{marker}\n{marker} two\n{code}\n"),
                lang
            )
            .len(),
            1
        );
        assert_eq!(
            failures(
                &format!("{}{code}\n", format!("{marker} text\n").repeat(6)),
                lang
            )
            .len(),
            1
        );
        assert!(
            failures(
                &format!(
                    "{marker}\n{}{marker}\n{code}\n",
                    format!("{marker} text\n").repeat(5)
                ),
                lang
            )
            .is_empty()
        );
        if lang != Language::Gitignore {
            assert_eq!(
                failures(&format!("{code} {marker} trailing\n"), lang).len(),
                1
            );
        }
    }
}

#[test]
fn shebang_bom_crlf_and_attributes() {
    for (lang, source) in [
        (
            Language::Shell,
            "#!/usr/bin/env bash\n# Shell owner.\necho ok\n",
        ),
        (
            Language::Python,
            "#!/usr/bin/env python3\n# Python owner.\nx = 1\n",
        ),
        (
            Language::Rust,
            "#!/usr/bin/env rust-script\n//! Rust owner.\n#![allow(dead_code)]\nfn main() {}\n",
        ),
        (
            Language::Rust,
            "\u{feff}\r\n//! Owner.\r\n#![allow(dead_code)]\r\n",
        ),
        (Language::Python, "\u{feff}# Owner.\nx = 1\n"),
    ] {
        assert!(failures(source, lang).is_empty(), "{source}");
    }
    assert_eq!(
        failures("#![allow(dead_code)]\n//! too late\n", Language::Rust).len(),
        1
    );
    assert_eq!(
        failures("use std::fmt;\n//! too late\n", Language::Rust).len(),
        1
    );
    assert_eq!(failures("#![doc = \"# prose\"]\n", Language::Rust).len(), 1);
}

#[test]
fn rust_literals_and_lifetimes_are_not_comments() {
    let source = r####"
fn f<'a>(x: &'a str) {
    let a = "http://example.test/*not*/ # text";
    let b = r###"// raw /* string */ \""###;
    let c = br#"// byte raw"#;
    let d = b"// bytes";
    let e = '/';
    let quote = '\'';
    let byte = b'/';
    let cstr = c"// #";
    let raw_cstr = cr#"/* // #"#;
    let generated = "# Settings\n# Default keys\n";
    let compose = r#"# Composition
# Template
"#;
}
"####;
    assert!(comments(source, Language::Rust).is_empty());
}

#[test]
fn rust_nested_blocks_are_one_violation() {
    let source = "fn f() { /* outer /* nested */ tail */ }";
    assert_eq!(
        comments(source, Language::Rust),
        vec![(Kind::Block, "/* outer /* nested */ tail */".into())]
    );
    assert_eq!(failures(source, Language::Rust).len(), 1);
    assert!(scan("/* unclosed", Language::Rust).is_err());
    assert!(scan("let x = r#\"unclosed", Language::Rust).is_err());
}

#[test]
fn rust_doc_kinds_and_inline_clusters_match_inventory() {
    let source = "//! owns\n//! one\n\n/// item\n/// two\nfn f() {}\n// a\n// b\n//// ordinary\n/** doc */\n/*** ordinary */\n";
    let got = comments(source, Language::Rust);
    assert_eq!(
        got.iter().map(|c| c.0).collect::<Vec<_>>(),
        [
            Kind::InnerDoc,
            Kind::OuterDoc,
            Kind::Line,
            Kind::OuterBlockDoc,
            Kind::Block
        ]
    );
    assert_eq!(got[2].1, "// a\n// b\n//// ordinary");
    for source in [
        "/// item\nfn f() {}",
        "/** docs */",
        "/*! docs */",
        "// ordinary",
        "//// ordinary",
    ] {
        assert_eq!(failures(source, Language::Rust).len(), 1);
    }
    assert_eq!(
        comments(
            "fn f() {\n let a=1; // one\n let b=2; // two\n}\n",
            Language::Rust
        )
        .len(),
        2
    );
}

#[test]
fn prose_attributes_include_cfg_attr_and_literal_macro_bodies() {
    for source in [
        "#[doc=\"docs\"] fn f() {}",
        "#![doc = include_str!(\"../README.md\")]",
        "#![cfg_attr(feature=\"x\", cfg_attr(feature=\"y\", doc = \"help\"))]",
        "macro_rules! m { () => { #[doc = \"help\"] fn f() {} }; }",
        "macro_rules! m { ($text:literal) => { #[doc = $text] fn f() {} }; }",
        "#[r#doc = \"help\"] fn f() {}",
    ] {
        assert_eq!(comments(source, Language::Rust)[0].0, Kind::DocAttribute);
        assert_eq!(failures(source, Language::Rust).len(), 1);
    }
    assert_eq!(
        comments("#[doc = /* token */ \"help\"]", Language::Rust)
            .iter()
            .map(|c| c.0)
            .collect::<Vec<_>>(),
        [Kind::DocAttribute, Kind::Block]
    );
    for source in [
        "#[doc(hidden)] fn f() {}",
        r#"#[doc(cfg(doc = "metadata"))] fn f() {}"#,
        r#"#[custom(doc = "argument")] fn f() {}"#,
        "#[doc(alias = \"name\")] fn f() {}",
        "#![no_std]",
        "#[expect(clippy::too_many_arguments, reason = \"// explicit boundary\")] fn f() {}",
        "macro_rules! m { ($attr:meta) => { #[$attr] fn f() {} }; }",
        "let text = \"#[doc = prose]\";",
    ] {
        assert!(comments(source, Language::Rust).is_empty());
    }
}

#[test]
fn python_docstrings_unicode_and_literals_match_inventory() {
    let source = "#!/usr/bin/env python3\n\"\"\"module docs\nsecond line\"\"\"\ns = \"# no\" # yes\né = \"é\" # unicode\nclass X:\n    \"\"\"class docs\"\"\"\n    def f(self):\n        \"\"\"function docs\"\"\"\n        return \"\"\"not docs # text\"\"\"\n";
    let got = comments(source, Language::Python);
    assert_eq!(
        got.iter().map(|c| c.0).collect::<Vec<_>>(),
        [
            Kind::Docstring,
            Kind::Hash,
            Kind::Hash,
            Kind::Docstring,
            Kind::Docstring
        ]
    );
    assert_eq!(got[2].1, "# unicode");
    assert_eq!(failures(source, Language::Python).len(), 5);
}

#[test]
fn python_docstrings_concatenate_and_include_async_functions() {
    for source in [
        "('module ' '# docs')\n",
        "def f():\n    ('function ' 'docs')\n",
        "async def f():\n    'async docs'\n",
        "# Owner.\n\"\"\"still forbidden\"\"\"\n",
    ] {
        assert_eq!(failures(source, Language::Python).len(), 1, "{source}");
    }
}

#[test]
fn python_all_strings_and_fstring_formats_are_data() {
    let source = r####"
x = '# single'
y = "# double"
z = r'\# raw'
b = b'# bytes'
br = br'\# bytes'
triple = '''# triple
# text'''
double_triple = """# triple
# text"""
f = f"{1:#x} # literal"
rf = rf"\# {1:#x}"
escaped = "\" # literal"
"""ordinary expression outside a docstring position"""
"####;
    assert!(comments(source, Language::Python).is_empty());
    let got = comments(
        "\"\"\"docs # prose\"\"\" # trailing\ns = f\"{1:#x} # literal\"\n",
        Language::Python,
    );
    assert_eq!(
        got.iter().map(|c| c.0).collect::<Vec<_>>(),
        [Kind::Docstring, Kind::Hash]
    );
    assert!(scan("x = '''unterminated", Language::Python).is_err());
}

#[test]
fn python_fstring_expression_comments_are_source_comments() {
    assert_eq!(
        comments(
            "x = f\"\"\"{1 # expression comment\n}\"\"\"\n",
            Language::Python
        )
        .len(),
        1
    );
}

#[test]
fn toml_strings_and_escapes_match_inventory() {
    let source = r####"# header
x = "# no" # yes
y = '''# raw
# raw too'''
z = """# multi
# still string"""
literal = '# literal'
quote = "quote \" # still string" # comment
multi = """escaped \""" # still multiline
end""" # tail
"####;
    assert_eq!(
        comments(source, Language::Toml)
            .iter()
            .map(|c| c.1.as_str())
            .collect::<Vec<_>>(),
        ["# header", "# yes", "# comment", "# tail"]
    );
    assert!(scan("x = \"unterminated", Language::Toml).is_err());
}

#[test]
fn shell_strings_parameters_and_heredocs_match_inventory() {
    let source = r####"#!/bin/bash
# header
echo '# quoted' "# double" foo#word "${#array[@]}" $# ${#x} ${x#prefix} # comment
cat <<'EOF_DATA'
# heredoc content, not shell comment
EOF_DATA
cat <<-DATA
	# tab-stripped heredoc
	DATA
echo \#escaped $'# ansi quoted'
cat <<< '# here string'
"####;
    assert_eq!(
        comments(source, Language::Shell)
            .iter()
            .map(|c| c.1.as_str())
            .collect::<Vec<_>>(),
        ["# header", "# comment"]
    );
}

#[test]
fn shell_substitutions_contain_real_comments() {
    let source = "x=\"$(\n# actual comment\nprintf '# data'\n)\"\n";
    assert_eq!(comments(source, Language::Shell).len(), 1);
    assert!(scan("echo 'unterminated", Language::Shell).is_err());
}

#[test]
fn gitignore_only_first_column_unescaped_hash_is_a_comment() {
    assert_eq!(
        comments(
            "# heading\n\\#literal\nfile#name\n #pattern\n",
            Language::Gitignore
        ),
        vec![(Kind::Hash, "# heading".into())]
    );
}

#[test]
fn make_recipe_and_assignment_hashes_match_inventory() {
    let source = "all:\n\techo \"# string\" # recipe comment\nX = a\\#literal # actual\n";
    assert_eq!(
        comments(source, Language::Make)
            .iter()
            .map(|c| c.1.as_str())
            .collect::<Vec<_>>(),
        ["# recipe comment", "# actual"]
    );
    assert_eq!(
        comments(
            "# heading\nX = a # inline\nall:\n\techo ok\n",
            Language::Make
        )
        .len(),
        2
    );
}

#[test]
fn make_continuations_keep_shell_quotes_and_hash_parameters() {
    let source = "all:\n\t@echo \"first \\\n\t# still a string\" $$# $${#x} $#; \\\n\t echo done # actual\nX = one \\\n two # assignment\n# comment continued \\\n and continued\n";
    let got = comments(source, Language::Make);
    assert_eq!(
        got.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(),
        [
            "# actual",
            "# assignment",
            "# comment continued \\\n and continued"
        ]
    );
}

#[test]
fn make_custom_prefix_and_inline_recipe() {
    let source = ".RECIPEPREFIX := >\nall:\n>@echo \"# literal\" $$# # actual\nother: ; echo '# quoted' # inline\n";
    assert_eq!(
        comments(source, Language::Make)
            .iter()
            .map(|c| c.1.as_str())
            .collect::<Vec<_>>(),
        ["# actual", "# inline"]
    );
    assert!(scan(".RECIPEPREFIX := $(PREFIX)\n", Language::Make).is_err());
}

#[test]
fn make_define_bodies_are_variable_data_and_oneshell_keeps_context() {
    let source = "define SCRIPT\n# data\nprintf '# data'\ndefine NESTED\n# data\nendef\nendef\nall:\n\t$(SCRIPT) # actual\n";
    assert_eq!(
        comments(source, Language::Make)
            .iter()
            .map(|c| c.1.as_str())
            .collect::<Vec<_>>(),
        ["# actual"]
    );
    let source = ".ONESHELL:\nall:\n\tcat <<'DATA'\n\t# heredoc data\n\tDATA\n\techo ok # actual\n";
    assert_eq!(
        comments(source, Language::Make)
            .iter()
            .map(|c| c.1.as_str())
            .collect::<Vec<_>>(),
        ["# actual"]
    );
}

#[test]
fn make_define_directives_accept_whitespace_separators() {
    for separator in [" ", "\t", " \t ", "\u{000b}", "\u{000c}", "\r"] {
        for modifier in [
            String::new(),
            format!("override{separator}"),
            format!("export{separator}"),
            format!("override{separator}export{separator}"),
        ] {
            let source = format!(
                "{modifier}define{separator}X\n# literal\ndefine{separator}INNER\n# nested literal\nendef{separator}\n# literal after nested\nendef{separator}# actual\n"
            );
            assert_eq!(
                comments(&source, Language::Make),
                vec![(Kind::Hash, "# actual".into())],
                "{source}"
            );
        }
    }
}

#[test]
fn make_recipe_prefixed_define_delimiters_are_literal_body_data() {
    for (setup, prefix) in [("", "\t"), (".RECIPEPREFIX := >\n", ">")] {
        for body in [
            format!("{prefix}endef\n# literal\n"),
            format!("{prefix}endef\t# literal on endef\n# literal\n"),
            format!("{prefix}define\tINNER\n# literal\n"),
        ] {
            let source = format!("{setup}define X\n{body}endef\t# actual\n");
            assert_eq!(
                comments(&source, Language::Make),
                vec![(Kind::Hash, "# actual".into())],
                "{source}"
            );
        }
        let source = format!("{setup}define X\n{prefix}endef\n# literal\n");
        assert!(scan(&source, Language::Make).is_err(), "{source}");
    }
    let source = ".RECIPEPREFIX := >\ndefine X\n# literal\n\tendef\t# actual\n";
    assert_eq!(
        comments(source, Language::Make),
        vec![(Kind::Hash, "# actual".into())]
    );
}

#[test]
fn make_directive_arguments_do_not_start_inline_recipes() {
    for separator in [" ", "\t", " \t "] {
        for keyword in [
            "ifdef", "ifndef", "ifeq", "ifneq", "else", "endif", "include", "-include", "sinclude",
            "override", "export",
        ] {
            let source = format!("{keyword}{separator}name:; '# actual\n");
            assert_eq!(
                comments(&source, Language::Make),
                vec![(Kind::Hash, "# actual".into())],
                "{source}"
            );
        }
        let source = format!("define{separator}name:; '\n# literal\nendef\n");
        assert!(comments(&source, Language::Make).is_empty(), "{source}");
    }
}

#[test]
fn make_directive_names_in_recipes_still_use_shell_comment_syntax() {
    for (setup, prefix) in [("", "\t"), (".RECIPEPREFIX := >\n", ">")] {
        for keyword in [
            "define", "endef", "ifdef", "ifeq", "else", "endif", "include", "override", "export",
        ] {
            let source = format!("{setup}all:\n{prefix}{keyword}\t'# literal' # actual\n");
            assert_eq!(
                comments(&source, Language::Make),
                vec![(Kind::Hash, "# actual".into())],
                "{source}"
            );
            let source = format!("{keyword}_target: ; echo '# literal' # actual\n");
            assert_eq!(
                comments(&source, Language::Make),
                vec![(Kind::Hash, "# actual".into())],
                "{source}"
            );
        }
    }
}

#[test]
fn allowlist_schema_rejects_unknown_missing_blank_and_duplicate_fields() {
    assert!(Allowlist::parse("entries = []").is_ok());
    for source in [
        "unknown = []",
        "entries = []\nunknown = true",
        "[[entries]]\npath='x.rs'\ncomment='// comment'\nreason='why'\nextra=true",
        "[[entries]]\npath='x.rs'\ncomment='// comment'",
        "[[entries]]\npath='x.rs'\ncomment='// comment'\nreason=' '",
        "[[entries]]\npath='../x.rs'\ncomment='// comment'\nreason='why'",
        "[[entries]]\npath='Cargo.lock'\ncomment='# comment'\nreason='why'",
        "[[entries]]\npath=1\ncomment='// comment'\nreason='why'",
    ] {
        assert!(Allowlist::parse(source).is_err(), "{source}");
    }
    let entry = "[[entries]]\npath='x.rs'\ncomment='// comment'\nreason='why'\n";
    assert!(Allowlist::parse(&entry.repeat(2)).is_err());
}

#[test]
fn allowlist_is_exact_and_staleness_obeys_scope() {
    let list = Allowlist::parse(
        "[[entries]]\npath='crates/x.rs'\ncomment='// comment'\nreason='external contract'\n",
    )
    .unwrap();
    let mut used = BTreeSet::new();
    assert!(!list.exempt("crates/x.rs", "// changed", &mut used));
    assert_eq!(list.stale(&used, &[]).len(), 1);
    assert!(list.stale(&used, &["tools".into()]).is_empty());
    assert_eq!(list.stale(&used, &["crates".into()]).len(), 1);
    assert!(list.exempt("crates/x.rs", "// comment", &mut used));
    assert!(list.stale(&used, &[]).is_empty());
}

#[test]
fn scope_excludes_dependency_data_and_sensitive_paths() {
    for path in [
        "Cargo.lock",
        "x.json",
        "x.md",
        "x.snap",
        "x.ansi",
        "x.txt",
        "LICENSE",
        ".env",
        ".env.local",
        "secrets.toml",
        "credentials.py",
    ] {
        assert!(language(path).is_none(), "{path}");
    }
    assert!(language("src/main.rs").is_some());
    assert!(in_scope("crates/foo/src/lib.rs", &["crates/foo".into()]));
    assert!(!in_scope(
        "crates/foobar/src/lib.rs",
        &["crates/foo".into()]
    ));
}

#[test]
fn make_hashes_in_variable_references_are_literal() {
    let source = "X = $(subst #,,value) # actual\nY = $(call f,$(value #))\n";
    assert_eq!(
        comments(source, Language::Make),
        vec![(Kind::Hash, "# actual".into())]
    );
    assert!(scan("X = $(unclosed\n", Language::Make).is_err());
}

#[test]
fn make_endef_trailing_comment_is_outside_the_literal_body() {
    let source = "define SCRIPT\n# data\nendef # actual\n";
    assert_eq!(
        comments(source, Language::Make),
        vec![(Kind::Hash, "# actual".into())]
    );
}

#[test]
fn shebang_shaped_ignore_header_is_safe() {
    assert!(failures("#!/pattern\n# Owner.\n", Language::Gitignore).is_empty());
}

#[test]
fn bom_preserves_adjacent_header_lines_in_every_language() {
    for (lang, marker) in [
        (Language::Rust, "//!"),
        (Language::Shell, "#"),
        (Language::Python, "#"),
        (Language::Toml, "#"),
        (Language::Make, "#"),
        (Language::Gitignore, "#"),
    ] {
        let source = format!("\u{feff}{marker} Owner.\n{marker} More ownership.\n");
        assert_eq!(comments(&source, lang).len(), 1, "{lang:?}");
        assert!(failures(&source, lang).is_empty(), "{lang:?}");
    }
}

#[test]
fn make_assignments_take_precedence_over_inline_recipes() {
    for operator in ["=", ":=", "::=", ":::=", "?=", "+=", "!="] {
        for prefix in ["", "override ", "export ", "all: ", "all: private "] {
            let source = format!("{prefix}X {operator} before; echo '# hidden'\n");
            assert_eq!(
                comments(&source, Language::Make),
                vec![(Kind::Hash, "# hidden'".into())],
                "{source}"
            );
            assert_eq!(
                failures(&source, Language::Make),
                ["trailing comment"],
                "{source}"
            );
            let source = format!("{prefix}X {operator} before; (\n");
            assert!(comments(&source, Language::Make).is_empty(), "{source}");
        }
    }
    for source in [
        "all: ; X=1; echo '# literal' # actual\n",
        "all:: ; echo '# literal' # actual\n",
        "$(subst :=,x,all): ; echo '# literal' # actual\n",
    ] {
        assert_eq!(
            comments(source, Language::Make),
            vec![(Kind::Hash, "# actual".into())]
        );
    }
}

#[test]
fn oneshell_collects_blank_lines_and_inline_first_commands() {
    for first in ["all:\n\tif true; then\n", "all: ; if true; then\n"] {
        let source = format!(".ONESHELL:\n{first}\n\techo \"#\"\n\tfi\n");
        assert!(comments(&source, Language::Make).is_empty());
        let source = format!(
            ".ONESHELL:\n{first}\n# Make comment\n\techo \"#\" # shell comment\n\tfi\nother: ; echo '# literal'\n"
        );
        let atoms = scan(&source, Language::Make).unwrap();
        assert_eq!(
            atoms
                .iter()
                .map(|c| &source[c.start..c.end])
                .collect::<Vec<_>>(),
            ["# Make comment", "# shell comment"]
        );
        assert_eq!(failures(&source, Language::Make).len(), 2);
    }
    let source = ".ONESHELL:\n.RECIPEPREFIX := >\nall: ; cat <<'DATA'\n\n># heredoc data\n>DATA\n>echo ok # actual\n";
    assert_eq!(
        comments(source, Language::Make),
        vec![(Kind::Hash, "# actual".into())]
    );
}

#[test]
fn oneshell_applies_to_recipes_before_the_directive() {
    let source = "all: ; if true; then\n\n\techo '#'\n\tfi\n.ONESHELL :\n";
    assert!(comments(source, Language::Make).is_empty());
    let source = "define DATA\n.ONESHELL:\nendef\nall:\n\tif true; then\n\techo ok\n\tfi\n";
    assert!(scan(source, Language::Make).is_err());
}

#[test]
fn spaced_script_shebangs_do_not_consume_header_lines() {
    for (lang, marker, code) in [
        (Language::Shell, "#", "echo ok"),
        (Language::Python, "#", "x = 1"),
        (Language::Rust, "//!", "fn main() {}"),
    ] {
        for space in ["", " ", "\t", " \t "] {
            for bom in ["", "\u{feff}"] {
                let header = format!("{marker} Owns this file.\r\n").repeat(5);
                let source = format!("{bom}#!{space}/usr/bin/env script\r\n{header}{code}\r\n");
                assert!(failures(&source, lang).is_empty(), "{lang:?}: {source}");
                assert_eq!(comments(&source, lang).len(), 1);
            }
        }
    }
    for attribute in [
        "#![allow(dead_code)]",
        "#! [allow(dead_code)]",
        "#!\t[allow(dead_code)]",
    ] {
        let source = format!("{attribute}\n//! Late header.\n");
        assert_eq!(failures(&source, Language::Rust).len(), 1);
    }
}

#[test]
fn data_files_treat_shebang_shaped_lines_as_header_comments() {
    for lang in [Language::Toml, Language::Make, Language::Gitignore] {
        for first in ["#!/first-header", "#! /first-header", "#!anything"] {
            let source = format!("{first}\n\n# second-header\n");
            let atoms = scan(&source, lang).unwrap();
            assert_eq!(atoms.len(), 2, "{lang:?}: {source}");
            assert_eq!(atoms[1].line, 3);
            assert_eq!(
                failures(&source, lang),
                ["comment outside the leading file header"]
            );
            let source = format!("{first}\n{}", "# header\n".repeat(5));
            assert_eq!(
                failures(&source, lang),
                ["file header has 6 text lines; maximum is 5"]
            );
        }
    }
}
