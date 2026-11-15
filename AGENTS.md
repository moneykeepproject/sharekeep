# AGENTS.md — read this first

This file is for every contributor, human or AI agent. Read it completely before
writing code, docs or tests.

## 1. Clean-room protocol (mandatory, applies to every change)

ShareKeep is one of the MoneyKeep Apps, a clean-room implementation of personal and
small-business finance software. MoneyKeep's full protocol, with its role roster and its
list of prior-art projects, is in `docs/clean-room.md` of the MoneyKeep repository
(`moneykeepproject/moneykeep`). The short version, which you must follow here too:

- **Know your role for the task.** You are either a **spec writer** (may study target
  products and prior art through use and public docs; writes only specs) or an
  **implementer** (writes code from specs and public standards only). Never both
  for the same feature area.
- **Implementers never read** source code of commercial finance products, any
  proprietary product, or any project listed under "Prior art" in MoneyKeep's
  `docs/clean-room.md`, whatever its licence (GPL, AGPL, LGPL, BSL, source-available,
  MIT or BSD).
- **Never** decompile, disassemble or reverse-engineer target binaries or their
  proprietary file formats.
- **Never** copy trademarks of commercial finance products, or their icons, artwork,
  report layouts, help text or UI wording.
- **Never** scrape or automate bank websites (browser automation is out of scope:
  institutions' terms of use prohibit it).
- If you are an AI agent and you are given, or come across, forbidden material: stop,
  do not use it, and say so in your output.

Code may be copied from the other MoneyKeep Apps (MoneyKeep, CalcKeep, GroupKeep,
Form1099Keep, keep-launcher): they are this project's own work. ShareKeep copies rather
than depends on them, so it builds on its own.

## 2. Workflow

```sh
cargo fmt --all -- --check                  # formatting
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                      # the split engine, its property tests, the rule checks
cargo xtask ci                              # all of the above, then the workflow and dialog rules
cargo xtask workflows                       # only release.yml may use the project's own Mac
cargo xtask guards                          # file dialogs only through a dialogs.rs module
```

Run `cargo xtask ci` before every commit. It must pass.

## 3. Rules learned in the other Keep apps

- **Money is integer minor units** (`sk_core::Cents`). No floating point in any money path.
- **File dialogs on macOS.** A blocking `rfd::FileDialog` crashed MoneyKeep 0.1.1 on the
  Mac (its modal panel's nested run loop re-enters the windowing library, which aborts).
  Every file dialog goes through one `dialogs.rs` module that uses `rfd::AsyncFileDialog`
  on macOS; `cargo xtask guards` (also a test) refuses `rfd::` anywhere else. Creating a
  book never opens a file dialog: the hub keeps books in its own data folder.
- **Line endings.** `.gitattributes` keeps sources LF on Windows checkouts; tools and tests
  read files line by line.
- **Users never build anything.** Releases ship installers through keep-launcher.

## 4. The spec

The design spec is "ShareKeep Design Spec v0.1" (owner's document). The parts implemented
so far are summarised in `docs/specs/`; a change of behaviour starts with a change there.
