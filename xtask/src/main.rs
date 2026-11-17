//! ShareKeep developer tasks.
//!
//! ```text
//! cargo xtask ci          fmt check, clippy -D warnings, tests, then the two rule checks
//! cargo xtask workflows   only release.yml may name the project's own Mac (self-hosted)
//! cargo xtask guards      file dialogs only through a `dialogs.rs` module (the macOS lesson)
//! cargo xtask installer   the Windows setup (installer.rs)
//! ```

mod installer;
mod licences;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let root = workspace_root();
    let result = match task.as_str() {
        "ci" => ci(&root),
        "workflows" => report(check_workflows(&root)),
        "guards" => report(check_guards(&root)),
        "installer" => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            installer::installer(&root, &rest)
        }
        _ => {
            eprintln!("usage: cargo xtask <ci|workflows|guards|installer>");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask {task}: {e}");
            ExitCode::FAILURE
        }
    }
}

pub fn workspace_root() -> PathBuf {
    // At run time, not build time: a shared target folder may hold an xtask built from
    // another checkout.
    let manifest =
        std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into());
    Path::new(&manifest)
        .parent()
        .expect("xtask sits in the workspace")
        .to_path_buf()
}

fn report(problems: Vec<String>) -> Result<(), String> {
    if problems.is_empty() {
        return Ok(());
    }
    for p in &problems {
        eprintln!("  {p}");
    }
    Err(format!("{} problem(s)", problems.len()))
}

fn ci(root: &Path) -> Result<(), String> {
    let steps: [&[&str]; 3] = [
        &["fmt", "--all", "--", "--check"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--workspace", "--locked"],
    ];
    for args in steps {
        println!("== cargo {}", args.join(" "));
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let ok = Command::new(&cargo)
            .args(args)
            .current_dir(root)
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if !ok {
            return Err(format!("cargo {} failed", args[0]));
        }
    }
    println!("== workflows");
    report(check_workflows(root))?;
    println!("== guards");
    report(check_guards(root))
}

/// Only `release.yml` may run on the project's own Mac (a self-hosted runner), as in the
/// other Keep apps: a pull request must never reach it.
fn check_workflows(root: &Path) -> Vec<String> {
    let dir = root.join(".github/workflows");
    let mut problems = Vec::new();
    for path in files(&dir, &["yml", "yaml"]) {
        let Ok(text) = fs::read_to_string(&path) else {
            problems.push(format!("{}: can't be read", path.display()));
            continue;
        };
        let text = text.replace("\r\n", "\n");
        problems.extend(workflow_problems(&file_name(&path), &text));
    }
    problems
}

fn workflow_problems(name: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let names_mac = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .any(|l| l.contains("self-hosted"));
    if names_mac && name != "release.yml" {
        problems.push(format!("{name}: only release.yml may use self-hosted"));
    }
    if names_mac && name == "release.yml" {
        let code: String = text
            .lines()
            .map(|l| l.split('#').next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        if code.contains("pull_request") {
            problems.push(format!(
                "{name}: a workflow that uses self-hosted can't run on pull requests"
            ));
        }
    }
    problems
}

/// A blocking file dialog crashed MoneyKeep 0.1.1 on macOS (a nested run loop re-entering
/// the windowing library). Every dialog must go through a `dialogs.rs` module, which uses
/// the asynchronous sheet on macOS.
fn check_guards(root: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let mut sources = files(&root.join("crates"), &["rs"]);
    sources.extend(files(&root.join("apps"), &["rs"]));
    for path in sources {
        if file_name(&path) == "dialogs.rs" {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        problems.extend(dialog_problems(&path.display().to_string(), &text));
    }
    problems
}

fn dialog_problems(name: &str, text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let code = l.split("//").next().unwrap_or("");
            code.contains("rfd::") || code.contains("use rfd")
        })
        .map(|(i, _)| {
            format!(
                "{name}:{}: file dialogs only through a dialogs.rs module",
                i + 1
            )
        })
        .collect()
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if file_name(&p) != "target" {
                out.extend(files(&p, exts));
            }
        } else if p.extension().is_some_and(|x| exts.iter().any(|e| x == *e)) {
            out.push(p);
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_repository_follows_its_rules() {
        let root = workspace_root();
        assert_eq!(check_workflows(&root), Vec::<String>::new());
        assert_eq!(check_guards(&root), Vec::<String>::new());
    }

    #[test]
    fn self_hosted_outside_release_is_refused() {
        let ci = "jobs:\n  a:\n    runs-on: [self-hosted, macOS]\n";
        assert_eq!(workflow_problems("ci.yml", ci).len(), 1);
        let commented = "jobs:\n  a:\n    runs-on: ubuntu-latest # not self-hosted\n";
        assert!(workflow_problems("ci.yml", commented).is_empty());
        let release_pr = "on:\n  pull_request:\njobs:\n  a:\n    runs-on: [self-hosted]\n";
        assert_eq!(workflow_problems("release.yml", release_pr).len(), 1);
        let release = "on:\n  push:\n    tags: ['v*']\njobs:\n  a:\n    runs-on: [self-hosted]\n";
        assert!(workflow_problems("release.yml", release).is_empty());
    }

    #[test]
    fn dialogs_outside_the_dialogs_module_are_refused() {
        let bad = "fn pick() {\n    let f = rfd::FileDialog::new().pick_file();\n}\n";
        assert_eq!(dialog_problems("app.rs", bad).len(), 1);
        assert_eq!(
            dialog_problems("app.rs", "use rfd::AsyncFileDialog;\n").len(),
            1
        );
        let comment = "// never call rfd:: here\nfn ok() {}\n";
        assert!(dialog_problems("app.rs", comment).is_empty());
    }
}
