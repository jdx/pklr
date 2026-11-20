//! A module that amends a base keeps its own imports, even when the base
//! imports a module under the same name.

fn eval(dir_name: &str, files: &[(&str, &str)], main: &str) -> serde_json::Value {
    let dir = std::env::temp_dir().join(format!("pklr-{dir_name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, source) in files {
        std::fs::write(dir.join(name), source).unwrap();
    }
    let result = pklr::eval_to_json(&dir.join(main));
    let _ = std::fs::remove_dir_all(&dir);
    result.expect("evaluation failed")
}

const CONFIG: &str = "class Step { check: String? }\nsteps: Mapping<String, Step> = new {}\n";
const BUILTINS: &str = "import \"Config.pkl\"\n\
    newlines: Config.Step = new { check = \"n\" }\n\
    prettier: Config.Step = new { check = \"p\" }\n";
const MID: &str = "amends \"Config.pkl\"\nimport \"Builtins.pkl\"\n\
    steps { [\"a\"] = Builtins.newlines }\n";

#[test]
fn child_import_of_the_same_module_as_the_base_reads_other_fields() {
    let value = eval(
        "amend-import-fields",
        &[
            ("Config.pkl", CONFIG),
            ("Builtins.pkl", BUILTINS),
            ("mid.pkl", MID),
            (
                "hk.pkl",
                "amends \"mid.pkl\"\nimport \"Builtins.pkl\"\n\
                 steps { [\"b\"] = Builtins.prettier }\n",
            ),
        ],
        "hk.pkl",
    );
    assert_eq!(
        value,
        serde_json::json!({"steps": {"a": {"check": "n"}, "b": {"check": "p"}}})
    );
}
