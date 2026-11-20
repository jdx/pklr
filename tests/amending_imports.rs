//! A module that amends a base keeps its own imports, even when the base
//! imports a module under the same name.

#![cfg(feature = "native-io")]

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

#[test]
fn child_import_with_a_query_keeps_the_fields_it_reads() {
    let dir = std::env::temp_dir().join(format!("pklr-amend-import-query-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("mp")).unwrap();
    std::fs::write(dir.join("mp/Config.pkl"), CONFIG).unwrap();
    std::fs::write(
        dir.join("mp/Builtins.pkl"),
        BUILTINS.replace("Config.pkl", "modulepath:/Config.pkl"),
    )
    .unwrap();
    std::fs::write(
        dir.join("mid.pkl"),
        "amends \"modulepath:/Config.pkl\"\nimport \"modulepath:/Builtins.pkl\"\n\
         steps { [\"a\"] = Builtins.newlines }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("hk.pkl"),
        "amends \"mid.pkl\"\nimport \"modulepath:/Builtins.pkl?rev=1\"\n\
         steps { [\"b\"] = Builtins.prettier }\n",
    )
    .unwrap();
    let result = pklr::EvaluatorBuilder::new()
        .module_path([dir.join("mp")])
        .eval_to_json(&dir.join("hk.pkl"));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        result.expect("evaluation failed"),
        serde_json::json!({"steps": {"a": {"check": "n"}, "b": {"check": "p"}}})
    );
}
