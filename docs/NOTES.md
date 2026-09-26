# Notes

## Domain

The existing format and rendering contracts remain in
[INTERFACES.md](INTERFACES.md) and [FEATURE-MAP.md](FEATURE-MAP.md).
The comment cleanup will consolidate additional external facts here after
the corresponding code changes; this tooling step moves no existing prose.

## Technology

### Comment extraction

The checker ports the step-1 inventory's language handling and fourteen
fixture cases. Rust uses
[rust-analyzer's rustc lexer distribution](https://docs.rs/ra-ap-rustc_lexer/0.174.0/ra_ap_rustc_lexer/),
which distinguishes comments from raw, byte and C strings, characters and
lifetimes, and handles nested block comments. A token pass recognizes
literal documentation attributes, including nested `cfg_attr` and macro
definitions. It does not expand declarative or procedural macros: attributes
assembled entirely by expansion are outside this source check.

[Tree-sitter Bash](https://github.com/tree-sitter/tree-sitter-bash) and
[Tree-sitter TOML](https://github.com/tree-sitter-grammars/tree-sitter-toml)
provide comment ranges without interpreting literal content as comments.
Parse errors fail closed. Python uses its standard `tokenize` and `ast`
modules; tokenizer columns count Unicode characters while AST columns count
UTF-8 bytes. Python 3.12+ exposes comments inside f-string expressions to
the tokenizer. The helper runs in an isolated interpreter, reads source on
stdin and executes none of the scanned code.

Make recipes use the shell grammar after masking recipe prefixes, Make
variable references and doubled dollars without moving byte offsets.
Continued recipes preserve quote state; `.ONESHELL` preserves context over
adjacent recipe lines. Literal `.RECIPEPREFIX` values and inline target
recipes are covered. A dynamic recipe prefix fails extraction. Make
assignment comments honor escaped hashes and continuations; quotes outside
recipes do not protect hashes. Hashes within Make variable/function references
and `define` bodies are literal data, as specified by the
[GNU Make manual](https://www.gnu.org/software/make/manual/html_node/Makefile-Contents.html).
Like heredocs and generated-text literals, that data is
not recursively interpreted, nor are Make variable/eval expansions run.

Adjacent standalone line comments of the same kind form one cluster;
trailing comments stay separate. Counts use inclusive physical line spans,
including blank documentation lines, matching the audit. Policy checks
count only the header's text lines. These are different measurements.

### PTY soak harness

`tools/soak.py` starts the player with `pty.fork()`, which makes the new
pty the child's controlling terminal. Only then does `TIOCSWINSZ` on the
master deliver a real `SIGWINCH`, so each storm resize reaches the player
exactly as a user resizing a terminal window would. RSS samples come from
`VmRSS` in `/proc/<pid>/status`, which exists on Linux only; on macOS the
harness still storms and validates escape streams but `rss.csv` holds only
its header. The RSS slope (< 1 MB/h after warmup, PLAN M5) is reported for a
reviewer, not gated by the harness exit code.

### Memory-mapped assets

The player, `RenderSession`, compositions, the CLI and the factory's eval
open `.ascii` assets as read-only private `memmap2` mappings. A mapping's
address does not change when its `Mmap` handle moves, so slices into it
outlive moves of the handle. Read-only and private only stop this process
writing through the mapping: another process can still modify or truncate
the file, and touching a page past a truncation raises `SIGBUS`. Replacing
an asset by rename is safe, because the old inode stays mapped; rewriting
it in place is not.

### GIF frame timing

GIF frame delays count hundredths of a second, so the review reel's delay
is a whole number of centiseconds and never below 10 ms: rates above
100 fps play at 100 fps.
