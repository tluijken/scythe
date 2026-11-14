#![cfg(not(windows))]

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use serde_json::Value;

const TARGETS: [&str; 4] =
    ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-gnu", "aarch64-apple-darwin", "x86_64-apple-darwin"];

/// Packages that declare only `license_file`, each checked by hand.
const LICENCE_FILE_ALLOW: &[(&str, &str)] = &[];

fn forces_gpl(expr: &str) -> bool {
    let normalised = expr.replace('/', " OR ").replace('(', " ( ").replace(')', " ) ");
    let tokens: Vec<&str> = normalised.split_whitespace().collect();
    let mut pos = 0;
    let forced = parse_or(&tokens, &mut pos);
    assert_eq!(pos, tokens.len(), "unparsed tokens in licence expression {expr:?}");
    forced
}

fn parse_or(tokens: &[&str], pos: &mut usize) -> bool {
    let mut forced = parse_and(tokens, pos);
    while tokens.get(*pos) == Some(&"OR") {
        *pos += 1;
        forced &= parse_and(tokens, pos);
    }
    forced
}

fn parse_and(tokens: &[&str], pos: &mut usize) -> bool {
    let mut forced = parse_leaf(tokens, pos);
    while tokens.get(*pos) == Some(&"AND") {
        *pos += 1;
        forced |= parse_leaf(tokens, pos);
    }
    forced
}

fn parse_leaf(tokens: &[&str], pos: &mut usize) -> bool {
    let token = *tokens.get(*pos).expect("licence expression ended early");
    *pos += 1;
    if token == "(" {
        let forced = parse_or(tokens, pos);
        assert_eq!(tokens.get(*pos), Some(&")"), "unbalanced parenthesis");
        *pos += 1;
        forced
    } else {
        if tokens.get(*pos) == Some(&"WITH") {
            *pos += 2;
        }
        token.contains("GPL")
    }
}

#[test]
fn a_licence_expression_forces_gpl_only_without_a_permissive_way_out() {
    assert!(!forces_gpl("MIT OR Apache-2.0"));
    assert!(!forces_gpl("MIT OR Apache-2.0 OR LGPL-2.1-or-later"));
    assert!(!forces_gpl("(MIT OR Apache-2.0) AND Unicode-3.0"));
    assert!(!forces_gpl("MIT/Apache-2.0"));
    assert!(forces_gpl("GPL-3.0"));
    assert!(forces_gpl("GPL-3.0-or-later"));
    assert!(forces_gpl("LGPL-2.1-only"));
    assert!(forces_gpl("AGPL-3.0"));
    assert!(forces_gpl("MIT AND GPL-2.0"));
    assert!(forces_gpl("(MIT OR GPL-3.0) AND LGPL-3.0"));
}

fn metadata(target: &str) -> Value {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--filter-platform", target])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo metadata");
    assert!(output.status.success(), "cargo metadata failed for {target}: {}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
}

fn shipped_offenders(meta: &Value) -> BTreeSet<String> {
    let packages: BTreeMap<&str, &Value> =
        meta["packages"].as_array().unwrap().iter().map(|p| (p["id"].as_str().unwrap(), p)).collect();
    let workspace: BTreeSet<&str> =
        meta["workspace_members"].as_array().unwrap().iter().map(|id| id.as_str().unwrap()).collect();
    let nodes: BTreeMap<&str, &Value> =
        meta["resolve"]["nodes"].as_array().unwrap().iter().map(|n| (n["id"].as_str().unwrap(), n)).collect();

    let root = *packages.iter().find(|(_, p)| p["name"] == "scyeq-plugin").expect("scyeq-plugin package").0;

    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for dep in nodes[id]["deps"].as_array().unwrap() {
            let ships =
                dep["dep_kinds"].as_array().unwrap().iter().any(|k| k["kind"].is_null() || k["kind"] == "build");
            if ships {
                stack.push(dep["pkg"].as_str().unwrap());
            }
        }
    }

    let mut offenders = BTreeSet::new();
    for id in seen.into_iter().filter(|id| !workspace.contains(id)) {
        let package = packages[id];
        let name = package["name"].as_str().unwrap();
        let version = package["version"].as_str().unwrap();
        match package["license"].as_str() {
            Some(expr) if forces_gpl(expr) => {
                offenders.insert(format!("{name} {version}: {expr}"));
            }
            Some(_) => {}
            None if LICENCE_FILE_ALLOW.iter().any(|(n, _)| *n == name) => {}
            None => {
                offenders.insert(format!("{name} {version}: no license field"));
            }
        }
    }
    offenders
}

#[test]
fn no_dependency_forces_a_gpl_licence() {
    let mut offenders = BTreeSet::new();
    for target in TARGETS {
        for offender in shipped_offenders(&metadata(target)) {
            offenders.insert(format!("[{target}] {offender}"));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}
