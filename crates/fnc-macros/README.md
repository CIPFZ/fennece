# fnc-macros

`#[derive(Settings)]` for `fnc_core::settings::Settings`.

Each field carries `#[setting(key = "...", default = <expr>, page = "...")]`; the derive generates
`Default`, the static descriptor table (key, page, doc comment, default source, kind) and key-based
`get`/`set`/`reset`, which drive the Options → Advanced page (UI-08) and `/key=value` commands.

**Invariants**: keys are unique (compile error otherwise); every field must be annotated.
