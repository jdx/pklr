use super::*;

// ============================================================
// HTTP URL rewriting
// ============================================================

#[test]
fn rewrite_url_longest_prefix_wins() {
    let mut ev = Evaluator::new_async();
    ev.set_http_rewrites(&[
        "https://example.com/=https://mirror.local/".to_string(),
        "https://example.com/special/=https://special.local/".to_string(),
    ]);
    // Longest prefix should win
    assert_eq!(
        ev.rewrite_url("https://example.com/special/foo.pkl"),
        "https://special.local/foo.pkl"
    );
    // Shorter prefix matches the rest
    assert_eq!(
        ev.rewrite_url("https://example.com/other/bar.pkl"),
        "https://mirror.local/other/bar.pkl"
    );
    // No match returns original
    assert_eq!(
        ev.rewrite_url("https://other.com/foo.pkl"),
        "https://other.com/foo.pkl"
    );
}

#[test]
fn rewrite_url_no_rules_is_identity() {
    let ev = Evaluator::new_async();
    assert_eq!(
        ev.rewrite_url("https://example.com/foo.pkl"),
        "https://example.com/foo.pkl"
    );
}

#[test]
fn class_instance_rejects_wrong_property_type() {
    let message = eval_fails(
        r#"
class Factory {
    enabled: Boolean = false
}

factory = new Factory { enabled = "yes" }
"#,
    );
    assert!(message.contains("enabled"), "{message}");
    assert!(message.contains("Boolean"), "{message}");
}

#[test]
fn class_instance_rejects_value_outside_string_literal_union() {
    let message = eval_fails(
        r#"
class Factory {
    version: "3" | "4" = "4"
}

factory = new Factory { version = "5" }
"#,
    );
    assert!(message.contains("version"), "{message}");
    assert!(message.contains("\"3\"|\"4\""), "{message}");
}

#[test]
fn class_instance_validates_hidden_property_types() {
    let message = eval_fails(
        r#"
class Factory {
    hidden enabled: Boolean = false
}

factory = new Factory { enabled = "yes" }
"#,
    );
    assert!(message.contains("enabled"), "{message}");
    assert!(message.contains("Boolean"), "{message}");
}

#[test]
fn class_instance_does_not_validate_missing_property_against_enclosing_scope() {
    let json = eval(
        r#"
local enabled = "not a boolean"

class Factory {
    hidden enabled: Boolean
}

factory = new Factory {}
"#,
    );
    assert_eq!(json["factory"], serde_json::json!({}));
}

// ============================================================
// output.renderer.converters
// ============================================================

#[test]
fn converter_injects_type_tag() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toMap().toDynamic()
            }
        }
    }
}

myStep = new Step {
    check = "cargo test"
}
"#,
    );
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["check"], "cargo test");
}

#[test]
fn converter_applies_to_amended_instance() {
    // Amending an instance preserves its class identity, so the class-keyed
    // converter still matches the amended value.
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toMap().toDynamic()
            }
        }
    }
}

local base = new Step { check = "a" }
myStep = (base) { check = "b" }
"#,
    );
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["check"], "b");
}

#[test]
fn converter_applies_to_subclass_instance() {
    let json = eval_with_converters(
        r#"
class Factory {
    fixed output = new { check = "base" }
}
class Prettier extends Factory {}

output {
    renderer {
        converters {
            [Factory] = (factory) -> factory.output
        }
    }
}

step = new Prettier {}
"#,
    );
    assert_eq!(json["step"]["check"], "base");
    assert!(json["step"].get("output").is_none());
}

#[test]
fn converter_prefers_most_specific_class() {
    let json = eval_with_converters(
        r#"
abstract class Factory {}
class Prettier extends Factory {}

output {
    renderer {
        converters {
            [Factory] = (_) -> "factory"
            [Prettier] = (_) -> "prettier"
        }
    }
}

value = new Prettier {}
"#,
    );
    assert_eq!(json["value"], "prettier");
}

#[test]
fn converter_can_call_module_local_helper() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

local function renderStep(step) = new Dynamic {
    _type = "step"
    ...step.toMap().toDynamic()
}

output {
    renderer {
        converters {
            [Step] = (step) -> renderStep(step)
        }
    }
}

step = new Step { check = "cargo test" }
"#,
    );
    assert_eq!(json["step"]["_type"], "step");
    assert_eq!(json["step"]["check"], "cargo test");
}

#[test]
fn converter_can_call_output_local_helper() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    local function renderStep(step) = new Dynamic {
        _type = "step"
        ...step.toMap().toDynamic()
    }
    renderer {
        converters {
            [Step] = (step) -> renderStep(step)
        }
    }
}

step = new Step { check = "cargo test" }
"#,
    );
    assert_eq!(json["step"]["_type"], "step");
    assert_eq!(json["step"]["check"], "cargo test");
}

#[test]
fn hk_style_factories_support_options_step_amendments_and_containers() {
    let json = eval_with_converters(
        r#"
open class Step {
    check: String = ""
    batch: Boolean = false
}

abstract class BuiltinFactory {
    step: Step
}

class Gitleaks extends BuiltinFactory {
    staged: Boolean = false
    local factory = this
    step = new Step {
        check = if (factory.staged) "gitleaks --staged" else "gitleaks"
    }
}

local gitleaks = new Gitleaks {}

plain = gitleaks
configured = (gitleaks) {
    staged = true
    step { batch = true }
}
steps: Mapping<String, BuiltinFactory | Step> = new Mapping {
    ["factory"] = (gitleaks) { staged = true }
    ["manual"] = new Step { check = "cargo test" }
}
all: Mapping<String, BuiltinFactory> = new Mapping {
    ["gitleaks"] = gitleaks
}

output {
    local function renderStep(step) = new Dynamic {
        _type = "step"
        ...step.toMap().toDynamic()
    }
    renderer {
        converters {
            [BuiltinFactory] = (factory) -> renderStep(factory.step)
            [Step] = (step) -> renderStep(step)
        }
    }
}
"#,
    );

    assert_eq!(json["plain"]["check"], "gitleaks");
    assert_eq!(json["plain"]["batch"], false);
    assert_eq!(json["plain"]["_type"], "step");
    assert_eq!(json["configured"]["check"], "gitleaks --staged");
    assert_eq!(json["configured"]["batch"], true);
    assert_eq!(json["configured"]["_type"], "step");
    assert_eq!(json["steps"]["factory"]["check"], "gitleaks --staged");
    assert_eq!(json["steps"]["manual"]["check"], "cargo test");
    assert_eq!(json["all"]["gitleaks"]["check"], "gitleaks");
    assert!(json["configured"].get("staged").is_none());
    assert!(json["configured"].get("step").is_none());
}

#[test]
fn hk_style_factory_rejects_unknown_input() {
    let message = eval_fails(
        r#"
class Step {}
abstract class BuiltinFactory { step: Step }
class Prettier extends BuiltinFactory { step = new Step {} }
prettier = new Prettier { futureOption = true }
"#,
    );
    assert!(message.contains("futureOption"), "{message}");
    assert!(message.contains("non-open"), "{message}");
}

#[test]
fn hidden_property_assigned_in_body_is_not_rendered() {
    // Apple Pkl excludes `hidden` properties from rendered output and from
    // `toMap()` whether the value comes from the class default or from an
    // assignment in a `new` body or an amendment.
    let json = eval(
        r#"
class Step {
    hidden staged: Boolean = false
    name: String = "x"
    label: String = if (staged) "staged" else "worktree"
}
constructed = new Step { staged = true }
amended = (new Step {}) { staged = true }
reamended = (constructed) { name = "y" }
asMap = constructed.toMap()
class Hook { steps: Mapping<String, Step> = new Mapping<String, Step> {} }
hook = new Hook { steps { ["s"] { staged = true } } }
"#,
    );
    for key in ["constructed", "amended", "reamended", "asMap"] {
        assert!(json[key].get("staged").is_none(), "{key}: {}", json[key]);
        assert_eq!(json[key]["label"], "staged", "{key}");
    }
    assert_eq!(json["reamended"]["name"], "y");
    assert!(
        json["hook"]["steps"]["s"].get("staged").is_none(),
        "{}",
        json["hook"]
    );
    assert_eq!(json["hook"]["steps"]["s"]["label"], "staged");
}

#[test]
fn typed_mapping_subclass_default_late_binds_body_assignments() {
    let json = eval(
        r#"
open class Step {
    hidden staged: Boolean = false
    name: String = "x"
    label: String = if (staged) "staged" else "worktree"
}
class SpecializedStep extends Step { extra: Int = 1 }
steps: Mapping<String, Step> = new Mapping<String, Step> {
    default = new SpecializedStep {}
    ["s"] { staged = true }
}
"#,
    );
    assert_eq!(
        json["steps"]["s"],
        serde_json::json!({"name": "x", "label": "staged", "extra": 1})
    );
}

#[test]
fn typed_new_entry_ignores_customized_mapping_default() {
    let json = eval(
        r#"
open class Step { name: String = "x"; batch: Boolean = false }
class SpecializedStep extends Step { extra: Int = 1 }
steps: Mapping<String, Step> = new Mapping<String, Step> {
    default = new SpecializedStep { batch = true }
    ["fresh"] = new Step {}
    ["amended"] { name = "y" }
}
"#,
    );
    assert_eq!(
        json["steps"]["fresh"],
        serde_json::json!({"name": "x", "batch": false})
    );
    assert_eq!(
        json["steps"]["amended"],
        serde_json::json!({"name": "y", "batch": true, "extra": 1})
    );
}

#[test]
fn typed_mapping_alias_default_late_binds_body_assignments() {
    let json = eval(
        r#"
class Step {
    hidden staged: Boolean = false
    label: String = if (staged) "staged" else "worktree"
}
typealias StepAlias = Step
steps: Mapping<String, Step> = new Mapping<String, Step> {
    default = new StepAlias {}
    ["s"] { staged = true }
}
aliased: Mapping<String, StepAlias> = new Mapping<String, StepAlias> {
    default = new StepAlias {}
    ["s"] { staged = true }
}
"#,
    );
    assert_eq!(json["steps"]["s"], serde_json::json!({"label": "staged"}));
    assert_eq!(json["aliased"]["s"], serde_json::json!({"label": "staged"}));
}

#[test]
fn typed_mapping_entry_accepts_instance_built_through_alias() {
    let json = eval(
        r#"
class Step { name: String = "x" }
typealias StepAlias = Step
steps: Mapping<String, Step> = new Mapping<String, Step> {}
amended = (steps) { ["a"] = new StepAlias { name = "aliased" } }
"#,
    );
    assert_eq!(json["amended"]["a"], serde_json::json!({"name": "aliased"}));
}

#[test]
fn typed_mapping_entry_rejects_other_class_in_union_with_primitive() {
    let message = eval_fails(
        r#"
class Step { name: String = "x" }
class Other { y: Int = 1 }
other = new Other {}
steps: Mapping<String, Step | String> = new Mapping<String, Step> {}
amended = (steps) { ["o"] = other }
"#,
    );
    assert!(
        message.contains("Expected value of type `Step | String`"),
        "{message}"
    );
    assert!(message.contains("got type `Other`"), "{message}");
}

#[test]
fn typed_mapping_entry_stays_lenient_for_unresolved_alternatives() {
    for src in [
        r#"
class Step { name: String = "x" }
class Other { y: Int = 1 }
other = new Other {}
steps: Mapping<String, Step | Dynamic> = new Mapping<String, Step> {}
amended = (steps) { ["o"] = other }
"#,
        r#"
class Step { name: String = "x" }
class Other { y: Int = 1 }
other = new Other {}
steps: Mapping<String, Step | Unknown.Alias> = new Mapping<String, Step> {}
amended = (steps) { ["o"] = other }
"#,
    ] {
        let json = eval(src);
        assert_eq!(json["amended"]["o"], serde_json::json!({"y": 1}), "{src}");
    }
}

#[test]
fn typed_mapping_entry_keeps_instance_of_declared_value_type() {
    // `= expr` entries whose value already is an instance of one of the
    // declared value types are stored as-is. The mapping's default template
    // (here the synthetic `new Step {}` from the typed literal) must not be
    // merged into a BuiltinFactory or a Group, and a type alias in the
    // annotation must be expanded before deciding that.
    let json = eval(
        r#"
open class Step { name: String = "x"; batch: Boolean = true }
class Group { dir: String = "" }
abstract class BuiltinFactory { step: Step }
class PrettierFactory extends BuiltinFactory { step = new Step { name = "prettier" } }
typealias StepDefinition = Step | BuiltinFactory
prettier = new PrettierFactory {}
class Hook { steps: Mapping<String, StepDefinition | Group> = new Mapping<String, Step> {} }
hooks: Mapping<String, Hook> = new Mapping<String, Hook> {
    ["check"] {
        steps {
            ["plain"] = prettier
            ["configured"] = (prettier) { step { batch = false } }
            ["group"] = new Group {}
            ["body"] { name = "custom" }
            ["typed"] = new Step { name = "t" }
        }
    }
}
"#,
    );
    let steps = &json["hooks"]["check"]["steps"];
    assert_eq!(
        steps["plain"],
        serde_json::json!({"step": {"name": "prettier", "batch": true}})
    );
    assert_eq!(
        steps["configured"],
        serde_json::json!({"step": {"name": "prettier", "batch": false}})
    );
    assert_eq!(steps["group"], serde_json::json!({"dir": ""}));
    assert_eq!(
        steps["body"],
        serde_json::json!({"name": "custom", "batch": true})
    );
    assert_eq!(
        steps["typed"],
        serde_json::json!({"name": "t", "batch": true})
    );
}

#[test]
fn typed_mapping_entry_rejects_instance_of_other_class() {
    // Apple Pkl: "Expected value of type `Step`, but got type `Other`."
    for src in [
        r#"
class Step { name: String = "x" }
class Other { y: Int = 1 }
class Hook { steps: Mapping<String, Step> = new Mapping<String, Step> {} }
hook = new Hook { steps { ["o"] = new Other {} } }
"#,
        r#"
class Step { name: String = "x" }
class Other { y: Int = 1 }
other = new Other {}
steps: Mapping<String, Step> = new Mapping<String, Step> {}
amended = (steps) { ["o"] = other }
"#,
    ] {
        let message = eval_fails(src);
        assert!(
            message.contains("Expected value of type `Step`"),
            "{message}"
        );
        assert!(message.contains("got type `Other`"), "{message}");
    }
}

#[test]
fn typed_mapping_entry_still_merges_template_into_untyped_values() {
    let json = eval(
        r#"
class Step { name: String = "x" }
steps: Mapping<String, Step> = new Mapping<String, Step> {}
amended = (steps) {
    ["dynamic"] = new Dynamic { extra = 1 }
    ["anonymous"] = new { extra = 2 }
}
"#,
    );
    // `new Dynamic { ... }` is a fresh object like any other `new T { ... }`.
    assert_eq!(json["amended"]["dynamic"], serde_json::json!({"extra": 1}));
    assert_eq!(
        json["amended"]["anonymous"],
        serde_json::json!({"extra": 2, "name": "x"})
    );
}

#[test]
fn converter_coerces_values() {
    let json = eval_with_converters(
        r#"
class Step {
    depends: String|List<String> = ""
    stash: Boolean|String = false
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s
                    .toMap()
                    .mapValues((k, v) ->
                        if (k == "depends" && v is String)
                            List(v)
                        else if (k == "stash" && v is Boolean)
                            if (v) "git" else "none"
                        else
                            v
                    )
                    .toDynamic()
            }
        }
    }
}

myStep = new Step {
    depends = "other"
    stash = true
}
"#,
    );
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["depends"], serde_json::json!(["other"]));
    assert_eq!(json["myStep"]["stash"], "git");
}

#[test]
fn converter_to_dynamic_removes_type_metadata() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Step {
                ...s
                    .toMap()
                    .mapValues((k, v) -> v)
                    .toDynamic()
            }.toDynamic()
        }
    }
}

myStep = new Step {
    check = "cargo test"
}
"#,
    );
    assert_eq!(json["myStep"]["check"], "cargo test");
}

#[test]
fn converter_does_not_reconvert_its_root_result() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Step {
                check = "\(s.check)!"
            }
        }
    }
}

myStep = new Step {
    check = "cargo test"
}
"#,
    );
    assert_eq!(json["myStep"]["check"], "cargo test!");
}

#[test]
fn converter_can_chain_to_different_root_type() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

class RenderedStep {
    label: String = ""
}

output {
    renderer {
        converters {
            [Step] = (s) -> new RenderedStep {
                label = s.check
            }
            [RenderedStep] = (s) -> new Dynamic {
                rendered = s.label
            }
        }
    }
}

myStep = new Step {
    check = "cargo test"
}
"#,
    );
    assert_eq!(json["myStep"]["rendered"], "cargo test");
}

#[test]
fn converter_multiple_types() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

myGroup = new Group {
    steps {
        ["lint"] = new Step {
            check = "eslint"
        }
    }
}
"#,
    );
    assert_eq!(json["myGroup"]["_type"], "group");
    assert_eq!(json["myGroup"]["steps"]["lint"]["_type"], "step");
    assert_eq!(json["myGroup"]["steps"]["lint"]["check"], "eslint");
}

#[test]
fn converter_union_mapping_chooses_matching_default_type() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
    shared: Boolean = false
}

class Step {
    check: String = ""
    shared: Boolean = false
}

class Hook {
    steps: Mapping<String, Step | Group> = new Mapping<String, Step | Group> {
        default {
            shared = true
        }
    }
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

hook = new Hook {
    steps {
        ["group"] {
            steps {
                ["lint"] {
                    check = "eslint"
                }
            }
        }
        ["echo"] {
            check = "echo ok"
        }
    }
}
"#,
    );
    assert_eq!(json["hook"]["steps"]["group"]["_type"], "group");
    assert_eq!(json["hook"]["steps"]["group"]["shared"], true);
    assert_eq!(
        json["hook"]["steps"]["group"]["steps"]["lint"]["_type"],
        "step"
    );
    assert_eq!(
        json["hook"]["steps"]["group"]["steps"]["lint"]["check"],
        "eslint"
    );
    assert_eq!(json["hook"]["steps"]["echo"]["_type"], "step");
    assert_eq!(json["hook"]["steps"]["echo"]["shared"], true);
    assert_eq!(json["hook"]["steps"]["echo"]["check"], "echo ok");
}

#[test]
fn converter_union_mapping_layers_explicit_default_over_type_default() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
    shared: Boolean = false
}

class Step {
    check: String = ""
    shared: Boolean = false
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps = new Mapping<String, Step | Group> {
    default {
        shared = true
    }
    ["group"] {
        steps {
            ["lint"] {
                check = "eslint"
            }
        }
    }
    ["echo"] {
        check = "echo ok"
    }
}
"#,
    );
    assert_eq!(json["steps"]["group"]["_type"], "group");
    assert_eq!(json["steps"]["group"]["shared"], true);
    assert_eq!(json["steps"]["echo"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["shared"], true);
}

#[test]
fn converter_union_mapping_preserves_explicit_new_type() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
    shared: Boolean = false
}

class Step {
    check: String = ""
    shared: Boolean = false
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps = new Mapping<String, Step | Group> {
    default {
        shared = true
    }
    ["group"] = new Group {
        steps {
            ["lint"] {
                check = "eslint"
            }
        }
    }
    ["echo"] {
        check = "echo ok"
    }
}
"#,
    );
    assert_eq!(json["steps"]["group"]["_type"], "group");
    assert_eq!(json["steps"]["group"]["shared"], true);
    assert_eq!(json["steps"]["group"]["steps"]["lint"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["shared"], true);
}

#[test]
fn converter_union_mapping_preserves_variable_value_type_after_default_merge() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
    shared: Boolean = false
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

local echo = new Step {
    check = "echo ok"
}

steps = new Mapping<String, Dynamic | Step> {
    default {
        shared = true
    }
    ["echo"] = echo
}
"#,
    );
    assert_eq!(json["steps"]["echo"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["check"], "echo ok");
    assert_eq!(json["steps"]["echo"]["shared"], false);
}

#[test]
fn converter_union_mapping_explicit_new_validates_class_body() {
    let msg = eval_fails(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Step {
    check: String = ""
}

steps = new Mapping<String, Step | Group> {
    ["group"] = new Group {
        unknown = true
    }
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("unknown"));
}

#[test]
fn converter_union_mapping_explicit_default_does_not_open_new_body() {
    let msg = eval_fails(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Step {
    check: String = ""
}

steps = new Mapping<String, Step | Group> {
    default {
        extra = false
    }
    ["group"] = new Group {
        extra = true
    }
}
"#,
    );
    assert!(msg.contains("non-open"));
    assert!(msg.contains("extra"));
}

#[test]
fn converter_union_mapping_untyped_new_stays_untyped() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

class Step {
    check: String = ""
}

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps = new Mapping<String, Step | Group> {
    ["plain"] = new {
        steps {
            ["lint"] {
                check = "eslint"
            }
        }
    }
}
"#,
    );
    assert_eq!(json["steps"]["plain"]["_type"], serde_json::Value::Null);
    assert_eq!(json["steps"]["plain"]["steps"]["lint"]["check"], "eslint");
}

#[test]
fn converter_union_mapping_explicit_new_without_type_default_uses_constructor() {
    let json = eval_with_converters(
        r#"
class Group {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
    shared: Boolean = false
}

class Step {
    check: String = ""
}

typealias GroupAlias = Group

output {
    renderer {
        converters {
            [Group] = (g) -> new Dynamic {
                _type = "group"
                ...g.toDynamic()
            }
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}

steps = new Mapping<String, GroupAlias | Step> {
    default {
        shared = true
    }
    ["group"] = new Group {
        steps {
            ["lint"] {
                check = "eslint"
            }
        }
    }
}
"#,
    );
    assert_eq!(json["steps"]["group"]["_type"], "group");
    assert_eq!(json["steps"]["group"]["shared"], true);
    assert_eq!(json["steps"]["group"]["steps"]["lint"]["_type"], "step");
}

#[test]
fn mapping_amendment_preserves_type_aliases() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
}

class Hook {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

typealias StepAlias = Step

hook = new Hook {
    steps {
        ["echo"] {
            check = (new Step { check = "echo ok" } as StepAlias).check
        }
    }
}
"#,
    );
    assert_eq!(json["hook"]["steps"]["echo"]["check"], "echo ok");
}

#[test]
fn mapping_local_const_is_visible_to_dynamic_entries() {
    let json = eval(
        r#"
values = new Mapping<String, String> {
    local const prefix = "hello"
    ["message"] = "\(prefix), world"
}
"#,
    );
    assert_eq!(json["values"]["message"], "hello, world");
}

#[test]
fn mapping_local_function_is_visible_to_dynamic_entries() {
    let json = eval(
        r#"
values = new Mapping<String, String> {
    local const prefix = "value"
    local function wrap(s: String): String = "[\(prefix):\(s)]"
    ["message"] = wrap("ok")
}
"#,
    );
    assert_eq!(json["values"]["message"], "[value:ok]");
}

#[test]
fn mapping_local_const_wins_over_same_named_member_of_typed_entry() {
    // Pkl resolves a name in the lexically enclosing bodies before the
    // object's inherited members, so `after` is the local, not the
    // `StepTest.after` property (which is null), with or without amending
    // the enclosing object.
    let json = eval(
        r#"
class StepTest {
    before: String?
    after: String?
    write: Mapping<String, String> = new Mapping<String, String> {}
}
open class Step {
    glob: String?
    tests: Mapping<String, StepTest> = new Mapping<String, StepTest> {}
}
step = new Step {
    tests {
        local const after = "formatted"
        ["nested"] { write { ["a.json"] = after } }
        ["direct"] { before = after }
    }
}
amended = (step) { glob = "*.json" }
"#,
    );
    for name in ["step", "amended"] {
        let tests = &json[name]["tests"];
        assert_eq!(tests["nested"]["write"]["a.json"], "formatted", "{name}");
        assert_eq!(tests["direct"]["before"], "formatted", "{name}");
    }
    assert_eq!(json["amended"]["glob"], "*.json");
}

#[test]
fn only_members_declared_in_an_enclosing_body_shadow_inherited_members() {
    let json = eval(
        r#"
open class Inner {
    name: String = "inner default"
    label: String?
    seen: Any
}
open class Outer {
    name: String = "outer default"
    label: String? = "outer label"
    inner: Any
}
name = "module"
fromModule = new Inner { seen = name }
inherited = new Outer { inner = new Inner { seen = label } }
declared = new Outer {
    label = "outer body"
    inner = new Inner { seen = label }
}
fromLocal = new Outer {
    local label = "outer local"
    inner = new Inner { seen = label }
}
"#,
    );
    // A module property is declared in the module body.
    assert_eq!(json["fromModule"]["seen"], "module");
    // `Outer.label` is inherited by the outer object, so `label` resolves
    // through the inner object's implicit `this`.
    assert_eq!(json["inherited"]["inner"]["seen"], serde_json::Value::Null);
    assert_eq!(json["declared"]["inner"]["seen"], "outer body");
    assert_eq!(json["fromLocal"]["inner"]["seen"], "outer local");
}

#[test]
fn amended_typed_mapping_entry_resolves_outer_names_before_inherited_members() {
    let json = eval(
        r#"
class T { label: String?; seen: Any }
open class S {
    label: String? = "S inherited"
    m: Mapping<String, T> = new Mapping<String, T> { ["a"] { seen = "base" } }
}
local label = "outer"
declared = new S { m { ["a"] { seen = label } } }
amended = (new S {}) { m { ["a"] { seen = label } } }
"#,
    );
    assert_eq!(json["declared"]["m"]["a"]["seen"], "outer");
    assert_eq!(json["amended"]["m"]["a"]["seen"], "outer");
}

#[test]
fn replaced_member_still_belongs_to_the_body_that_declared_it() {
    // `b = a + 1` refers to the object's own `a`, not the module's, even
    // after a later amendment replaces `a`.
    let json = eval(
        r#"
a = 100
open class C { a: Int = 0; b: Int = 0 }
s = new C { a = 1; b = a + 1 }
s2 = (s) { a = 5 }
"#,
    );
    assert_eq!(json["s"]["b"], 2);
    assert_eq!(json["s2"]["b"], 6);
}

#[tokio::test]
async fn parent_class_entries_resolve_their_module_names_before_inherited_members() {
    let temp = TestTempDir::new("pklr_test_parent_class_entry_lexical_names");
    let dir = temp.path();
    std::fs::write(
        dir.join("Parent.pkl"),
        r#"
local const label = "parent module"
open class Inner { label: String?; seen: Any }
open class Parent { inner: Inner = new Inner { seen = label } }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "Parent.pkl"
class Child extends Parent.Parent { extra: Int = 0 }
plain = new Child {}
amended = new Child { extra = 1 }
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(json["plain"]["inner"]["seen"], "parent module");
    assert_eq!(json["amended"]["inner"]["seen"], "parent module");
}

#[tokio::test]
async fn import_resolves_before_same_named_inherited_member() {
    let temp = TestTempDir::new("pklr_test_import_before_inherited_member");
    let dir = temp.path();
    std::fs::write(dir.join("lib.pkl"), "x = \"import\"\n").unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "lib.pkl"
open class T { lib: Dynamic = new Dynamic { x = "member" }; seen: Any }
t = new T { seen = lib.x }
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(json["t"]["seen"], "import");
}

#[test]
fn class_body_member_is_lexical_to_its_nested_objects() {
    // `a` is written in `C`'s body, so the nested object reads the instance's
    // `a` rather than `Inner.a`, including after an amendment replaces it.
    let json = eval(
        r#"
open class Inner { a: Int = 0; seen: Any }
open class C { a: Int = 1; inner: Inner = new Inner { seen = a } }
plain = new C {}
amended = new C { a = 5 }
class D extends C { b: Int = 0 }
sub = new D { a = 7 }
"#,
    );
    assert_eq!(json["plain"]["inner"]["seen"], 1);
    assert_eq!(json["amended"]["inner"]["seen"], 5);
    assert_eq!(json["sub"]["inner"]["seen"], 7);
}

#[test]
fn member_inherited_from_a_parent_class_is_not_lexical_to_nested_objects() {
    // `Child`'s body does not declare `label`, so the nested object's own
    // inherited `label` wins over the one `Child` inherits from `Parent`.
    let json = eval(
        r#"
open class Inner { label: String? = "inner"; seen: Any }
open class Parent { label: String? = "parent" }
class Child extends Parent { inner: Inner = new Inner { seen = label } }
plain = new Child {}
amended = new Child { label = "amended" }
"#,
    );
    assert_eq!(json["plain"]["inner"]["seen"], "inner");
    assert_eq!(json["amended"]["inner"]["seen"], "inner");
}

#[test]
fn replaced_member_stays_in_its_body_across_later_amendments() {
    let json = eval(
        r#"
a = 100
open class Inner { a: Int = 0; seen: Any }
open class C { a: Int = 1; b: Int = 0; c: Int = 0; inner: Inner = new Inner { seen = a } }
s = new C { a = 5 }
s2 = (s) { b = 2 }
s3 = (s2) { b = 3 }
t = (new C {}) { a = 1; b = a + 1 }
t2 = (t) { a = 7 }
t3 = (t2) { c = 3 }
"#,
    );
    // `a` is declared in `C`'s body, so its nested object keeps reading the
    // instance's `a` however many amendments follow the one that replaced it.
    for name in ["s", "s2", "s3"] {
        assert_eq!(json[name]["inner"]["seen"], 5, "{name}");
    }
    // Likewise `b = a + 1` keeps reading its own body's `a`, not the module's.
    assert_eq!(json["t2"]["b"], 8);
    assert_eq!(json["t3"]["b"], 8);
}

#[test]
fn member_added_by_an_amendment_does_not_shadow_the_definitions_scope() {
    // The amendment's `x` is not declared in `e`'s body, so `e`'s nested
    // object still reads the module's `x`, before `Inner.x`.
    let json = eval(
        r#"
local const x = "module"
open class Inner { x: String = "inner member"; seen: Any }
e = new Dynamic { inner = new Inner { seen = x } }
e2 = (e) { x = "overlay" }
"#,
    );
    assert_eq!(json["e"]["inner"]["seen"], "module");
    assert_eq!(json["e2"]["inner"]["seen"], "module");
}

#[tokio::test]
async fn shared_import_stays_declared_when_amending_an_imported_object() {
    // The amending module imports `lib.pkl` too, so the two views of the
    // import are merged. The merged binding is still `defs.pkl`'s import and
    // must keep resolving before `Inner.lib`.
    let temp = TestTempDir::new("pklr_test_shared_import_stays_declared");
    let dir = temp.path();
    std::fs::write(dir.join("lib.pkl"), "x = \"import\"\n").unwrap();
    std::fs::write(
        dir.join("defs.pkl"),
        r#"
import "lib.pkl"
open class Inner { lib: Any = "member"; seen: Any }
open class C {
    extra: Int = 0
    inner: Inner = new Inner { seen = if (extra > 0) lib.x else "none" }
}
c = new C {}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.pkl"),
        r#"
import "lib.pkl"
import "defs.pkl"
result = (defs.c) { extra = 1 }
"#,
    )
    .unwrap();

    let json = pklr::eval_to_json_async(&dir.join("main.pkl"))
        .await
        .unwrap();
    assert_eq!(json["result"]["inner"]["seen"], "import");
}

#[test]
fn mapping_local_lambda_is_visible_to_sibling_local() {
    // A lambda local must be in scope for a later (non-lambda) local that uses it.
    let json = eval(
        r#"
values = new Mapping<String, Int> {
    local f = (k, v) -> v * 2
    local doubled = (new Mapping<String, Int> { ["a"] = 1 }).toMap().mapValues(f).toMapping()
    ["out"] = doubled["a"]
}
"#,
    );
    assert_eq!(json["values"]["out"], 2);
}

#[test]
fn mapping_local_lambda_is_visible_to_sibling_local_untyped() {
    // Same, for an untyped `new Mapping {}` body (different eval path).
    let json = eval(
        r#"
values = new Mapping {
    local f = (x) -> x + 1
    local g = f.apply(10)
    ["out"] = g
}
"#,
    );
    assert_eq!(json["values"]["out"], 11);
}

#[test]
fn object_local_lambda_does_not_capture_later_local_for_early_call() {
    let msg = eval_fails(
        r#"
values {
    local f = (x) -> x + h
    local g = f.apply(1)
    local h = 42
    out = g
}
"#,
    );
    assert!(msg.contains("undefined variable: h"), "{msg}");
}

#[test]
fn mapping_local_lambda_does_not_capture_later_local_for_early_call() {
    let msg = eval_fails(
        r#"
values = new Mapping {
    local f = (x) -> x + h
    local g = f.apply(1)
    local h = 42
    ["out"] = g
}
"#,
    );
    assert!(msg.contains("undefined variable: h"), "{msg}");
}

#[test]
fn mapping_local_body_is_visible_to_dynamic_entries() {
    let json = eval(
        r#"
values = new Mapping<String, Int> {
    local options = new Dynamic {
        port = 3000
    }
    ["port"] = options.port
}
"#,
    );
    assert_eq!(json["values"]["port"], 3000);
}

#[test]
fn single_type_mapping_amendment_preserves_default_template() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
    enabled: Boolean = false
}

class Hook {
    steps: Mapping<String, Step> = new Mapping<String, Step> {
        default {
            enabled = true
        }
    }
}

hook = new Hook {
    steps {
        ["echo"] {
            check = "echo ok"
        }
    }
}
"#,
    );
    assert_eq!(json["hook"]["steps"]["echo"]["check"], "echo ok");
    assert_eq!(json["hook"]["steps"]["echo"]["enabled"], true);
}

#[test]
fn mapping_entry_body_recomputes_late_bound_type_properties() {
    let json = eval_with_converters(
        r#"
class Step {
    check: String = ""
    label: String = "Step: \(check)"
    enabled: Boolean = false
}

steps = new Mapping<String, Step> {
    default {
        enabled = true
    }
    ["lint"] {
        check = "eslint"
    }
}
"#,
    );
    assert_eq!(json["steps"]["lint"]["check"], "eslint");
    assert_eq!(json["steps"]["lint"]["label"], "Step: eslint");
    assert_eq!(json["steps"]["lint"]["enabled"], true);
}

#[test]
fn converter_no_converters_is_noop() {
    let json = eval_with_converters(
        r#"
x = 1
y = "hello"
"#,
    );
    assert_eq!(json["x"], 1);
    assert_eq!(json["y"], "hello");
}

#[test]
fn converter_output_not_in_result() {
    let json = eval_with_converters(
        r#"
class Foo {
    x: Int = 0
}

output {
    renderer {
        converters {
            [Foo] = (f) -> new Dynamic {
                _type = "foo"
                ...f.toDynamic()
            }
        }
    }
}

item = new Foo { x = 42 }
"#,
    );
    assert!(json.get("output").is_none());
    assert_eq!(json["item"]["_type"], "foo");
    assert_eq!(json["item"]["x"], 42);
}

#[test]
fn converter_inherited_from_amends_base() {
    use std::io::Write;
    let dir =
        std::env::temp_dir().join(format!("pklr_test_amends_converter_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Base module with class + converter
    let base_path = dir.join("Base.pkl");
    let mut base_file = std::fs::File::create(&base_path).unwrap();
    write!(
        base_file,
        r#"
class Step {{
    check: String = ""
}}

output {{
    renderer {{
        converters {{
            [Step] = (s) -> new Dynamic {{
                _type = "step"
                ...s.toDynamic()
            }}
        }}
    }}
}}
"#
    )
    .unwrap();

    // Amending module
    let child_path = dir.join("child.pkl");
    let mut child_file = std::fs::File::create(&child_path).unwrap();
    write!(
        child_file,
        r#"amends "Base.pkl"

myStep = new Step {{
    check = "cargo test"
}}
"#
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let val = ev
            .eval_source(&std::fs::read_to_string(&child_path).unwrap(), &child_path)
            .await
            .unwrap();
        let val = ev.apply_converters(val).await.unwrap();
        val.to_json()
    });
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["check"], "cargo test");
}

#[test]
fn converter_inherited_from_extends_base() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!(
        "pklr_test_extends_converter_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let base_path = dir.join("Base.pkl");
    let mut base_file = std::fs::File::create(&base_path).unwrap();
    write!(
        base_file,
        r#"
open class Step {{
    check: String = ""
}}

output {{
    renderer {{
        converters {{
            [Step] = (s) -> new Dynamic {{
                _type = "step"
                ...s.toDynamic()
            }}
        }}
    }}
}}
"#
    )
    .unwrap();

    let child_path = dir.join("child.pkl");
    let mut child_file = std::fs::File::create(&child_path).unwrap();
    write!(
        child_file,
        r#"extends "Base.pkl"

myStep = new Step {{
    check = "make test"
}}
"#
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let val = ev
            .eval_source(&std::fs::read_to_string(&child_path).unwrap(), &child_path)
            .await
            .unwrap();
        let val = ev.apply_converters(val).await.unwrap();
        val.to_json()
    });
    assert_eq!(json["myStep"]["_type"], "step");
    assert_eq!(json["myStep"]["check"], "make test");
}

/// Simple: amends with bare object in a typed Mapping property
#[test]
fn converter_amends_simple_typed_mapping() {
    let json = eval_with_converters(
        r#"
open class Step {
    check: String = ""
}

steps: Mapping<String, Step> = new Mapping<String, Step> {
    ["echo"] {
        check = "echo ok"
    }
}

output {
    renderer {
        converters {
            [Step] = (s) -> new Dynamic {
                _type = "step"
                ...s.toDynamic()
            }
        }
    }
}
"#,
    );
    eprintln!(
        "simple JSON: {}",
        serde_json::to_string_pretty(&json).unwrap()
    );
    assert_eq!(json["steps"]["echo"]["_type"], "step");
    assert_eq!(json["steps"]["echo"]["check"], "echo ok");
}

/// Reproduces the hk pattern: base defines classes + typed Mappings + converters,
/// child amends with bare object bodies (no `new Step`).
#[test]
fn converter_amends_bare_object_in_mapping() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("pklr_test_bare_mapping_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let base_path = dir.join("Config.pkl");
    let mut f = std::fs::File::create(&base_path).unwrap();
    write!(
        f,
        r#"
open class Step {{
    check: String = ""
    fix: String = ""
}}

open class Hook {{
    steps: Mapping<String, Step> = new Mapping<String, Step> {{}}
}}

hooks: Mapping<String, Hook> = new Mapping<String, Hook> {{}}

output {{
    renderer {{
        converters {{
            [Step] = (s) -> new Dynamic {{
                _type = "step"
                ...s.toDynamic()
            }}
        }}
    }}
}}
"#
    )
    .unwrap();

    let child_path = dir.join("hk.pkl");
    let mut f = std::fs::File::create(&child_path).unwrap();
    write!(
        f,
        r#"amends "Config.pkl"

hooks {{
    ["check"] {{
        steps {{
            ["echo"] {{
                check = "echo ok"
            }}
        }}
    }}
}}
"#
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt.block_on(async {
        let mut ev = Evaluator::new_async();
        let val = ev
            .eval_source(&std::fs::read_to_string(&child_path).unwrap(), &child_path)
            .await
            .unwrap();
        let val = ev.apply_converters(val).await.unwrap();
        val.to_json()
    });
    eprintln!("JSON: {}", serde_json::to_string_pretty(&json).unwrap());
    assert_eq!(json["hooks"]["check"]["steps"]["echo"]["_type"], "step");
    assert_eq!(json["hooks"]["check"]["steps"]["echo"]["check"], "echo ok");
}

#[test]
fn package_amends_with_rewrite_inherits_hk_style_converter() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_package_rewrite_converter");
    let mut zip_bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Config.pkl", options).unwrap();
        zip.write_all(
            br#"
class Step {
    check: String = ""
}

class Hook {
    steps: Mapping<String, Step> = new Mapping<String, Step> {}
}

hooks: Mapping<String, Hook> = new Mapping<String, Hook> {}

output {
    renderer {
        converters {
            [Step] = (s) -> new Step {
                ...s
                    .toMap()
                    .mapValues((k, v) ->
                        if (k == "check")
                            "\(v)!"
                        else
                            v
                    )
                    .toDynamic()
            }.toDynamic()
        }
    }
}
"#,
        )
        .unwrap();
        zip.finish().unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            zip_bytes.len()
        )
        .unwrap();
        stream.write_all(&zip_bytes).unwrap();
    });

    let child_path = temp.path().join("hk.pkl");
    std::fs::write(
        &child_path,
        r#"
amends "package://example.com/v1.0.0/hk@1.0.0#/Config.pkl"

hooks {
    ["check"] {
        steps {
            ["echo"] {
                check = "echo ok"
            }
        }
    }
}
"#,
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt
        .block_on(async {
            pklr::eval_to_json_with_options_async(
                &child_path,
                pklr::AsyncEvalOptions {
                    http_rewrites: vec![format!("https://example.com/=http://{addr}/")],
                    ..Default::default()
                },
            )
            .await
        })
        .unwrap();
    server.join().unwrap();

    assert_eq!(json["hooks"]["check"]["steps"]["echo"]["check"], "echo ok!");
}

#[test]
fn package_cache_survives_across_offline_evaluators() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_persistent_package_cache");
    let cache_dir = temp.path().join("cache");
    let mut zip_bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file("Config.pkl", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"answer = 42\n").unwrap();
        zip.finish().unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            zip_bytes.len()
        )
        .unwrap();
        stream.write_all(&zip_bytes).unwrap();
    });

    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();
    let rewrite = format!("https://example.com/=http://{addr}/");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([rewrite.clone()])
                .package_cache_dir(cache_dir.clone())
                .eval_to_json(&config_path),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(first["answer"], 42);

    // A new evaluator succeeds after the one-shot server has shut down.
    let second = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([rewrite])
                .package_cache_dir(cache_dir)
                .offline(true)
                .eval_to_json(&config_path),
        )
        .unwrap();
    assert_eq!(second["answer"], 42);
}

/// Build a single-entry package zip holding `contents` at `name`.
#[cfg(feature = "package-zip-core")]
fn package_zip(name: &str, contents: &str) -> Vec<u8> {
    use std::io::Write;

    let mut bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    bytes
}

#[test]
fn preloaded_package_evaluates_offline_without_a_cold_start() {
    let temp = TestTempDir::new("pklr_test_preloaded_package");
    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();

    // No server is ever started: the preloaded zip is the only source.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .package_cache_dir(temp.path().join("cache"))
                .offline(true)
                .preload_package(
                    "https://example.com/pkg@1.0.0.zip",
                    "zip",
                    package_zip("Config.pkl", "answer = 42\n"),
                )
                .eval_to_json(&config_path),
        )
        .unwrap();
    assert_eq!(json["answer"], 42);
}

#[test]
fn preloaded_package_does_not_override_a_cached_download() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_preload_precedence");
    let cache_dir = temp.path().join("cache");
    let fetched = package_zip("Config.pkl", "answer = 42\n");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            fetched.len()
        )
        .unwrap();
        stream.write_all(&fetched).unwrap();
    });

    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();
    let rewrite = format!("https://example.com/=http://{addr}/");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(
        pklr::AsyncEvaluatorBuilder::new()
            .http_rewrites([rewrite])
            .package_cache_dir(cache_dir.clone())
            .eval_to_json(&config_path),
    )
    .unwrap();
    server.join().unwrap();

    // The downloaded package is already cached, so the preloaded copy is ignored.
    let json = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .package_cache_dir(cache_dir)
                .offline(true)
                .preload_package(
                    "https://example.com/pkg@1.0.0.zip",
                    "zip",
                    package_zip("Config.pkl", "answer = 7\n"),
                )
                .eval_to_json(&config_path),
        )
        .unwrap();
    assert_eq!(json["answer"], 42);
}

#[test]
fn preloading_a_package_for_another_version_is_a_cache_miss() {
    let temp = TestTempDir::new("pklr_test_preload_version_mismatch");
    let config_path = temp.path().join("config.pkl");
    // The config pins 2.0.0 while the preloaded package is 1.0.0.
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@2.0.0#/Config.pkl\"\n",
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let error = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .package_cache_dir(temp.path().join("cache"))
                .offline(true)
                .preload_package(
                    "https://example.com/pkg@1.0.0.zip",
                    "zip",
                    package_zip("Config.pkl", "answer = 42\n"),
                )
                .eval_to_json(&config_path),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("package is not cached and offline mode is enabled"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn preloading_invalid_package_bytes_is_rejected() {
    let temp = TestTempDir::new("pklr_test_preload_invalid");
    let mut evaluator = pklr::Evaluator::new_async();
    evaluator.set_package_cache_dir(temp.path().join("cache"));
    let error = evaluator
        .preload_package_async("https://example.com/pkg@1.0.0.zip", "zip", b"not a zip")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("package archive is invalid"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn preloading_reports_a_cache_write_failure() {
    let temp = TestTempDir::new("pklr_test_preload_write_failure");
    // A file where the cache directory belongs: the seed cannot be stored, and
    // reporting success would leave the host expecting a usable cache entry.
    let cache_path = temp.path().join("cache");
    std::fs::write(&cache_path, b"not a directory").unwrap();

    let mut evaluator = pklr::Evaluator::new_async();
    evaluator.set_package_cache_dir(&cache_path);
    let error = evaluator
        .preload_package_async(
            "https://example.com/pkg@1.0.0.zip",
            "zip",
            &package_zip("Config.pkl", "answer = 42\n"),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&cache_path.display().to_string()),
        "error should name the cache path: {error}"
    );
}

#[test]
fn direct_package_relatives_survive_across_offline_evaluators() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_direct_package_relative_cache");
    let cache_dir = temp.path().join("cache");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let body = if request.starts_with("GET /Config.pkl ") {
                "amends \"./Base.pkl\"\nanswer = 42\n"
            } else if request.starts_with("GET /Base.pkl ") {
                "base = 41\n"
            } else {
                panic!("unexpected request: {request}");
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });

    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://pkg.pkl-lang.org/github.com/acme/pkg@v1#/Config.pkl\"\n",
    )
    .unwrap();
    let rewrite = format!("https://github.com/acme/pkg/releases/download/v1/=http://{addr}/");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([rewrite.clone()])
                .package_cache_dir(cache_dir.clone())
                .eval_to_json(&config_path),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(first["base"], 41);
    assert_eq!(first["answer"], 42);

    let second = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([rewrite])
                .package_cache_dir(cache_dir)
                .offline(true)
                .eval_to_json(&config_path),
        )
        .unwrap();
    assert_eq!(second["base"], 41);
    assert_eq!(second["answer"], 42);
}

#[test]
fn unreadable_package_cache_is_a_miss_while_online() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let temp = TestTempDir::new("pklr_test_unreadable_package_cache");
    let cache_path = temp.path().join("cache");
    std::fs::write(&cache_path, "not a directory").unwrap();
    let mut zip_bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file("Config.pkl", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"answer = 42\n").unwrap();
        zip.finish().unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            zip_bytes.len()
        )
        .unwrap();
        stream.write_all(&zip_bytes).unwrap();
    });

    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let json = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .http_rewrites([format!("https://example.com/=http://{addr}/")])
                .package_cache_dir(cache_path)
                .eval_to_json(&config_path),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(json["answer"], 42);
}

#[test]
fn offline_package_cache_miss_is_actionable() {
    let temp = TestTempDir::new("pklr_test_offline_package_cache_miss");
    let config_path = temp.path().join("config.pkl");
    std::fs::write(
        &config_path,
        "amends \"package://example.com/pkg@1.0.0#/Config.pkl\"\n",
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let error = rt
        .block_on(
            pklr::AsyncEvaluatorBuilder::new()
                .package_cache_dir(temp.path().join("cache"))
                .offline(true)
                .eval_to_json(&config_path),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("package is not cached and offline mode is enabled"));
    assert!(error.contains("https://example.com/pkg@1.0.0.zip"));
}

#[test]
fn super_in_object_amendment_of_function_result() {
    let v = eval(
        r#"
class Step { check: String? }
local function run(cmd: String): Step = new { check = cmd }
local defs = new Mapping<String, Step> { ["a"] = (run("echo a")) { check = "\(super.check) b" } }
result = defs
"#,
    );
    assert_eq!(v["result"], serde_json::json!({"a":{"check":"echo a b"}}));
}

#[test]
fn super_in_object_amendment_chain_and_nested_bodies() {
    let v = eval(
        r#"
local base = new { value = "a"; child { value = "inner" } }
local middle = (base) { value = super.value + "b" }
result = (middle) { value = super.value + "c" }
inherited = (middle) { other = true }
nested = (base) { value = super.value + "!"; child { value = super.value + "?" } }
indexed = (base) { value = super["value"] + "i" }
"#,
    );
    assert_eq!(v["result"]["value"], "abc");
    assert_eq!(v["inherited"]["value"], "ab");
    assert_eq!(v["nested"]["value"], "a!");
    assert_eq!(v["nested"]["child"]["value"], "inner?");
    assert_eq!(v["indexed"]["value"], "ai");
}

#[test]
fn super_in_object_amendment_keeps_child_receiver() {
    let v = eval(
        r#"
local base = new { x = 1; y = x; z = this.x }
result = (base) { x = 2; y = super.y; z = super.z }
"#,
    );
    assert_eq!(v["result"], serde_json::json!({"x":2,"y":2,"z":2}));
}

#[test]
fn super_in_object_amendment_reads_amended_parent_member_once() {
    let v = eval(
        r#"
local base = new { child { value = "a" } }
local middle = (base) { child { value = super.value + "b" } }
result = (middle) { child = super.child }
"#,
    );
    assert_eq!(v["result"]["child"]["value"], "ab");
}

#[test]
fn super_in_mapping_keeps_parent_keys_and_receiver_metadata() {
    let v = eval(
        r#"
local base = new Mapping { ["a"] = 1; ["length"] = 42 }
result = (base) {
  ["b"] = super["a"]
  ["parentLengthKey"] = super["length"]
  ["count"] = super.length
  ["allKeys"] = super.keys
  ["empty"] = super.isEmpty
  ["last"] = 9
}
"#,
    );
    assert_eq!(v["result"]["b"], 1);
    assert_eq!(v["result"]["parentLengthKey"], 42);
    assert_eq!(v["result"]["count"], 8);
    assert_eq!(
        v["result"]["allKeys"],
        serde_json::json!([
            "a",
            "length",
            "b",
            "parentLengthKey",
            "count",
            "allKeys",
            "empty",
            "last"
        ])
    );
    assert_eq!(v["result"]["empty"], false);
}

#[test]
fn super_in_nested_collections_uses_their_own_parent() {
    let v = eval(
        r#"
local base = new {
  a = 99
  xs = new Listing { 1; 2 }
  m = new Mapping<String, Int> { ["a"] = 1 }
}
result = (base) {
  xs { [0] = super[1] }
  m { ["b"] = super["a"] }
  fresh = new { a = super.a }
}
"#,
    );
    assert_eq!(v["result"]["xs"], serde_json::json!([2, 2]));
    assert_eq!(v["result"]["m"], serde_json::json!({"a":1,"b":1}));
    assert_eq!(v["result"]["fresh"]["a"], serde_json::json!({}));
}

#[test]
fn super_declared_property_takes_precedence_over_builtin_name() {
    let v = eval(
        r#"
class Base { length: Int = 10 }
result = new Base { length = super.length + 1 }
"#,
    );
    assert_eq!(v["result"]["length"], 11);
}

#[test]
fn super_metadata_is_not_available_on_typed_objects() {
    for name in ["length", "keys", "isEmpty", "isNotEmpty"] {
        let source = format!(
            r#"
class Base {{ value: Int = 1; result: Any }}
result = new Base {{ result = super.{name} }}
"#
        );
        assert!(eval_fails(&source).contains(&format!("field not found: {name}")));
    }
}

#[test]
fn super_metadata_inside_generators_uses_complete_receiver() {
    let v = eval(
        r#"
local base = new Mapping { ["base"] = 1 }
result = (base) {
  for (name in List("one", "two")) {
    when (true) { [name] = super.length }
  }
  when (false) { ["notUsed"] = 0 } else { ["allKeys"] = super.keys }
  ["end"] = 2
}
"#,
    );
    assert_eq!(v["result"]["one"], 5);
    assert_eq!(v["result"]["two"], 5);
    assert_eq!(
        v["result"]["allKeys"],
        serde_json::json!(["base", "one", "two", "allKeys", "end"])
    );
}

#[test]
fn super_metadata_keeps_all_typed_mapping_amendment_keys() {
    let v = eval(
        r#"
local base = new Mapping<String, Any> { ["a"] = 1 }
local middle = (base) { ["b"] = 2 }
result = (middle) { ["count"] = super.length; ["allKeys"] = super.keys }
local object = new { m = middle }
nested = (object) { m { ["count"] = super.length; ["allKeys"] = super.keys } }
"#,
    );
    let expected = serde_json::json!({"a":1,"b":2,"count":4,"allKeys":["a","b","count","allKeys"]});
    assert_eq!(v["result"], expected);
    assert_eq!(v["nested"]["m"], expected);
}

#[test]
fn super_listing_metadata_uses_amended_receiver() {
    let v = eval(
        r#"
local base = new Listing { 1; 2 }
result = (base) { super.length; super.isEmpty }
local object = new { xs = base }
nested = (object) { xs { [0] = super.length } }
generated = (base) { for (i in List(1,2)) { when (true) { super.length } } }
"#,
    );
    assert_eq!(v["result"], serde_json::json!([1, 2, 4, false]));
    assert_eq!(v["nested"]["xs"], serde_json::json!([2, 2]));
    assert_eq!(v["generated"], serde_json::json!([1, 2, 4, 4]));
}

#[test]
fn super_metadata_does_not_evaluate_local_values_while_counting() {
    let v = eval(
        r#"
local base = new Listing { 1 }
result = (base) { local count = super.length; count }
unused = (base) { local count = super.length; super.length }
local mapping = new Mapping { ["a"] = 1 }
entries = (mapping) { local count = super.length; ["count"] = count }
"#,
    );
    assert_eq!(v["result"], serde_json::json!([1, 2]));
    assert_eq!(v["unused"], serde_json::json!([1, 2]));
    assert_eq!(v["entries"], serde_json::json!({"a":1,"count":2}));
}

#[test]
fn string_replace_last() {
    let v = eval(
        r#"
result = "a/*".replaceLast("*", "*.txt")
repeated = "ababa".replaceLast("aba", "X")
unicode = "é猫é猫".replaceLast("猫", "犬")
absent = "abc".replaceLast("z", "X")
emptyPattern = "abc".replaceLast("", "!")
emptyString = "".replaceLast("", "!")
removed = "abcabc".replaceLast("abc", "")
literal = "a.*b.*".replaceLast(".*", "$1")
"#,
    );
    assert_eq!(
        v,
        serde_json::json!({
            "result": "a/*.txt", "repeated": "abX", "unicode": "é猫é犬",
            "absent": "abc", "emptyPattern": "abc!", "emptyString": "!",
            "removed": "abc", "literal": "a.*b$1"
        })
    );
}

#[test]
fn string_replace_last_requires_string_arguments() {
    assert!(eval_fails(r#"result = "abc".replaceLast(1, "x")"#).contains("replaceLast"));
    assert!(eval_fails(r#"result = "abc".replaceLast("a", 1)"#).contains("replaceLast"));
    assert!(eval_fails(r#"result = "abc".replaceLast("a")"#).contains("replaceLast"));
}

#[test]
fn mapping_when_inside_for() {
    let v = eval(
        r#"
local cmds = new Mapping<String, String?> { ["a"] = "echo a"; ["b"] = null }
result = new Mapping<String, String> {
  for (name, cmd in cmds) {
    when (cmd != null) { [name] = cmd }
  }
}
"#,
    );
    assert_eq!(v["result"], serde_json::json!({"a": "echo a"}));
}

#[test]
fn mapping_when_branches_preserve_defaults_and_iteration_scope() {
    let v = eval(
        r#"
class Step { command: String; enabled: Boolean = true }
result = new Mapping<String, Step> {
  default { enabled = false }
  when (false) { ["unselected"] { command = missing } }
  for (name in List("a", "b")) {
    when (name == "a") {
      when (true) { [name] { command = "echo a" } }
    } else {
      for (suffix in List("1", "2")) {
        [name + suffix] { command = "echo " + name + suffix }
      }
    }
  }
}
"#,
    );
    assert_eq!(
        v["result"],
        serde_json::json!({
            "a": {"command": "echo a", "enabled": false},
            "b1": {"command": "echo b1", "enabled": false},
            "b2": {"command": "echo b2", "enabled": false}
        })
    );
}

#[test]
fn inferred_function_result_keeps_class_in_property() {
    let v = eval(
        r#"
class Step { check: String?; glob: String? }
class Holder { step: Step }
local function run(cmd: String): Step = new { check = cmd }
local holder = new Holder { step = run("echo a") }
result = (holder.step) { glob = "*" }
typed = holder.step is Step
"#,
    );
    assert_eq!(
        v["result"],
        serde_json::json!({"check":"echo a", "glob":"*"})
    );
    assert_eq!(v["typed"], true);
}

#[test]
fn inferred_function_result_uses_parameter_over_class_property() {
    let v = eval(
        r#"
class Step { stage: String?; check: String? }
local function precommit(stage: String): Step = new {
  check = "run --hook-stage \(stage)" + (if (stage == "pre-commit") " --files" else "")
}
result = precommit("pre-commit")
"#,
    );
    assert_eq!(v["result"]["check"], "run --hook-stage pre-commit --files");
}

#[test]
fn lambda_parameters_override_class_properties_across_call_forms() {
    let v = eval(
        r#"
class Step { stage: String?; check: String? }
local make = (stage) -> new Step { check = stage }
class Holder {
  function makeStep(stage: String): Step = new Step { check = stage }
}
local holder = new Holder {}
applied = make.apply("apply")
method = holder.makeStep("method")
callback = (new Listing { "callback" }).map(make)
piped = "pipe" |> make
"#,
    );
    assert_eq!(v["applied"]["check"], "apply");
    assert_eq!(v["method"]["check"], "method");
    assert_eq!(v["callback"][0]["check"], "callback");
    assert_eq!(v["piped"]["check"], "pipe");
}

#[test]
fn converter_parameter_overrides_class_property() {
    let v = eval_with_converters(
        r#"
class Step { check: String = "" }
class Converted { stage: String?; check: String? }
output {
  renderer {
    converters {
      [Step] = (stage) -> new Converted { check = stage.check }
    }
  }
}
item = new Step { check = "input" }
"#,
    );
    assert_eq!(v["item"]["check"], "input");
}

#[test]
fn inferred_function_result_keeps_members_through_mapping() {
    let v = eval(
        r#"
class Step { check: String?; glob: String?; summary = "cmd: \(check)" }
local function run(cmd: String): Step = new { check = cmd }
local steps = new Mapping<String, Step> { ["a"] = run("echo a"); ["b"] = run("echo b") }
result = (steps["a"]) { glob = "*" }
changed = (steps["b"]) { check = "echo c" }
typed = steps["a"] is Step
"#,
    );
    assert_eq!(
        v["result"],
        serde_json::json!({"check":"echo a", "glob":"*", "summary":"cmd: echo a"})
    );
    assert_eq!(
        v["changed"],
        serde_json::json!({"check":"echo c", "glob":null, "summary":"cmd: echo c"})
    );
    assert_eq!(v["typed"], true);
}

#[test]
fn inferred_function_result_branches_and_generic_types() {
    let v = eval(
        r#"
class Step { check: String? }
typealias Alias = Step
local function choose(flag: Boolean): Step = if (flag) new { check = "a" } else let (cmd = "b") new { check = cmd }
local function optional(): Step? = new { check = "optional" }
local function aliased(): Alias = new { check = "alias" }
local function listing(): Listing<String> = new { "one"; "two" }
local function mapping(): Mapping<String, Step> = new { ["a"] { check = "mapped" } }
a = choose(true)
b = choose(false)
c = optional()
d = aliased()
items = listing()
steps = mapping()
typed = steps["a"] is Step
"#,
    );
    assert_eq!(
        v,
        serde_json::json!({
            "a":{"check":"a"}, "b":{"check":"b"}, "c":{"check":"optional"},
            "d":{"check":"alias"}, "items":["one","two"],
            "steps":{"a":{"check":"mapped"}}, "typed":true
        })
    );
}

#[test]
fn inferred_function_result_union_default() {
    let v = eval(
        r#"
class Step { check: String? }
local function run(): *Step | String = new { check = "a" }
result = run()
typed = result is Step
"#,
    );
    assert_eq!(v["result"], serde_json::json!({"check":"a"}));
    assert_eq!(v["typed"], true);
    assert!(
        eval_fails(
            r#"
class Step { check: String? }
local function run(): Step | String = new { check = "a" }
result = run()
"#
        )
        .contains("Cannot tell which parent to amend")
    );
}

#[tokio::test]
async fn inferred_function_result_imported_class() {
    let dir = TestTempDir::new("inferred_function_result");
    std::fs::write(
        dir.path().join("types.pkl"),
        r#"
class Step { check: String?; glob: String? }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("factory.pkl"),
        r#"
import "types.pkl"
function run(cmd: String): types.Step = new { check = cmd }
"#,
    )
    .unwrap();
    let path = dir.path().join("main.pkl");
    let source = r#"
import "types.pkl"
import "factory.pkl"
class Holder { step: types.Step }
local holder = new Holder { step = factory.run("echo a") }
result = (holder.step) { glob = "*" }
typed = holder.step is types.Step
"#;
    let mut evaluator = Evaluator::new_async();
    let v = evaluator
        .eval_source(source, &path)
        .await
        .unwrap()
        .to_json();
    assert_eq!(
        v["result"],
        serde_json::json!({"check":"echo a", "glob":"*"})
    );
    assert_eq!(v["typed"], true);
}

#[test]
fn inferred_function_result_generic_aliases_and_union_defaults() {
    let v = eval(
        r#"
class Step { check: String?; glob: String? }
class Group { label: String }
typealias Steps = Mapping<String, Step>
typealias Items = Listing<String>
typealias Choice = *Step | String
local function aliases(): Steps = new { ["a"] { check = "alias" } }
local function items(): Items = new { "one" }
local function choice(): Choice = new { check = "choice" }
local function nullable(): Mapping<String, Step?> = new { ["a"] { check = "nullable" } }
local function union(): Mapping<String, *Step | Group> = new { ["a"] { check = "union" }; ["b"] = new Group { label = "group" } }
local function defaultList(): *Listing<String> | String = new { "default" }
local function defaultNullable(): *Step? | String = new { check = "default nullable" }
aliased = (aliases()["a"]) { glob = "*" }
optional = (nullable()["a"]) { glob = "*" }
combined = (union()["a"]) { glob = "*" }
list = items()
selected = choice()
defaults = defaultList()
optionalDefault = defaultNullable()
typed = union()["b"] is Group
"#,
    );
    assert_eq!(
        v["aliased"],
        serde_json::json!({"check":"alias", "glob":"*"})
    );
    assert_eq!(
        v["optional"],
        serde_json::json!({"check":"nullable", "glob":"*"})
    );
    assert_eq!(
        v["combined"],
        serde_json::json!({"check":"union", "glob":"*"})
    );
    assert_eq!(v["list"], serde_json::json!(["one"]));
    assert_eq!(v["defaults"], serde_json::json!(["default"]));
    assert_eq!(v["selected"]["check"], "choice");
    assert_eq!(v["optionalDefault"]["check"], "default nullable");
    assert_eq!(v["typed"], true);
}

#[test]
fn inferred_function_result_through_trace() {
    let v = eval(
        r#"
class Step { check: String?; glob: String? }
typealias Steps = Mapping<String, Step>
class Holder { step: Step }
local function run(): Step = trace(new { check = "a" })
local function steps(): Steps = trace(new { ["a"] = run() })
local holder = new Holder { step = run() }
result = (holder.step) { glob = "*" }
mapped = (steps()["a"]) { glob = "*" }
typed = holder.step is Step
"#,
    );
    assert_eq!(v["result"], serde_json::json!({"check":"a", "glob":"*"}));
    assert_eq!(v["mapped"], v["result"]);
    assert_eq!(v["typed"], true);
}

#[test]
fn inferred_function_mapping_union_keys_keep_value_type_slot() {
    let v = eval(
        r#"
class Step { check: String?; glob: String? }
typealias Key = String | Int
local function make(): Mapping<String | Int, Step> = new {
  ["a"] { check = "a" }
  [1] { check = "one" }
}
local function aliased(): Mapping<Key, Step> = new { ["a"] { check = "alias" } }
local original = make()
local extended = (original) { ["b"] { check = "b" } }
result = (extended["b"]) { glob = "*" }
first = (original["a"]) { glob = "*" }
typed = extended["b"] is Step
aliasTyped = aliased()["a"] is Step
"#,
    );
    assert_eq!(v["result"], serde_json::json!({"check":"b", "glob":"*"}));
    assert_eq!(v["first"], serde_json::json!({"check":"a", "glob":"*"}));
    assert_eq!(v["typed"], true);
    assert_eq!(v["aliasTyped"], true);
}

#[test]
fn mapping_value_amendments_bind_their_own_super() {
    let v = eval(
        r#"
class Item { x: Int; hidden secret = 7 }
local base = new Mapping<String, Item> { ["a"] { x = 1 } }
local middle = (base) { ["a"] { x = super.x + 1 } }
result = (middle) { ["a"] { x = super.x + super.secret } }
"#,
    );
    assert_eq!(v["result"], serde_json::json!({"a":{"x":9}}));
}

#[test]
fn listing_super_first_and_last_read_the_amended_receiver() {
    let v = eval(
        r#"
local base = new Listing { 1; 2 }
first = (base) { [0] = 3; super.first }
last = (base) { super.last; 4 }
generated = new Listing { for (x in List(7, 8)) { x }; super.first }
spread = new Listing { super.last; ...List(4, 5) }
"#,
    );
    assert_eq!(v["first"], serde_json::json!([3, 2, 3]));
    assert_eq!(v["last"], serde_json::json!([1, 2, 4, 4]));
    assert_eq!(v["generated"], serde_json::json!([7, 8, 7]));
    assert_eq!(v["spread"], serde_json::json!([5, 4, 5]));
    assert!(eval_fails("x = new Listing { super.last }").contains("maximum recursion depth"));
}

#[test]
fn listing_super_endpoints_do_not_force_unrelated_locals() {
    let v = eval(
        r#"
first = new Listing { 1; local x = super.first; x }
last = new Listing { local x = super.last; x; 2 }
local base = new Listing { 1 }
amended = (base) { super.last; local n = 7; n }
"#,
    );
    assert_eq!(v["first"], serde_json::json!([1, 1]));
    assert_eq!(v["last"], serde_json::json!([2, 2]));
    assert_eq!(v["amended"], serde_json::json!([1, 7, 7]));
}

#[test]
fn mapping_explicit_value_default_survives_entry_body() {
    let v = eval(
        r#"
class Item { shared: Boolean = false; name: String = "" }
result = new Mapping<String, Item> {
  default = new { shared = true; extra = 7 }
  ["a"] { name = "a" }
}
"#,
    );
    assert_eq!(
        v["result"]["a"],
        serde_json::json!({"shared":true,"name":"a","extra":7})
    );
}

#[test]
fn listing_super_endpoints_keep_locals_in_generator_bodies() {
    let v = eval(
        r#"
first = new Listing { local n = 7; for (x in List(1)) { n }; super.first }
last = new Listing { local n = 8; super.last; when (true) { n } }
"#,
    );
    assert_eq!(v["first"], serde_json::json!([7, 7]));
    assert_eq!(v["last"], serde_json::json!([8, 8]));
}

#[test]
fn listing_super_endpoint_index_amendments_keep_locals() {
    let v = eval(
        r#"
local base = new Listing { new { x = 1 } }
result = (base) { local n = 7; [0] { x = n }; super.first }
"#,
    );
    assert_eq!(v["result"], serde_json::json!([{"x":7},{"x":7}]));
}

#[test]
fn mapping_explicit_value_defaults_survive_further_amendments() {
    let v = eval(
        r#"
class Item { shared: Boolean = false; name: String = "" }
local base = new Mapping<String, Item> {
  default = new { shared = true; extra = 7 }
  ["a"] { name = "a" }
}
result = (base) { ["a"] { name = super.name + "b"; extra = super.extra + 1 } }
"#,
    );
    assert_eq!(
        v["result"]["a"],
        serde_json::json!({"shared":true,"name":"ab","extra":8})
    );
}

#[test]
fn mapping_explicit_defaults_keep_class_expressions_late_bound() {
    let v = eval(
        r#"
class Item { x: Int = 1; computed: Int = x + 1; shared: Boolean = false }
local base = new Mapping<String, Item> {
  default = new { shared = true; extra = 7 }
  ["a"] { x = 5 }
}
result = (base) { ["a"] { x = 9 } }
initial = base["a"]
"#,
    );
    assert_eq!(
        v["initial"],
        serde_json::json!({"x":5,"computed":6,"shared":true,"extra":7})
    );
    assert_eq!(
        v["result"]["a"],
        serde_json::json!({"x":9,"computed":10,"shared":true,"extra":7})
    );
}

#[test]
fn listing_super_endpoints_preserve_generator_shadowing() {
    let v = eval(
        r#"
result = new Listing { local n = 7; local m = n; for (n in List(2)) { m + n }; super.first }
"#,
    );
    assert_eq!(v["result"], serde_json::json!([9, 9]));
}

#[test]
fn lambda_object_reads_enclosing_bindings_through_outer() {
    let json = eval(
        r#"
local x = 42
local make = () -> new Dynamic {
  value = outer.x
  indexed = outer["x"]
}
result = make.apply()
"#,
    );
    assert_eq!(json["result"]["value"], 42);
    assert_eq!(json["result"]["indexed"], 42);
}

#[test]
fn lambda_object_local_reads_enclosing_binding_before_its_own() {
    let json = eval(
        r#"
local x = 41
local make = () -> new Dynamic {
  local y = x
  local x = 0
  value = y
}
result = make.apply()
"#,
    );
    assert_eq!(json["result"]["value"], 41);
}

#[test]
fn lambda_object_keeps_default_type_binding() {
    let json = eval(
        r#"
class Step { value = 42 }
class Holder { selected: *Step | String }
local make = () -> new Holder {}
result = make.apply()
"#,
    );
    assert_eq!(json["result"]["selected"]["value"], 42);
}

#[test]
fn lambda_keeps_quoted_class_name() {
    let json = eval(
        r#"
class `Step?` { value = 42 }
local make = () -> new `Step?` {}
result = make.apply()
"#,
    );
    assert_eq!(json["result"]["value"], 42);
}

#[test]
fn lambda_object_keeps_generic_default_type_binding() {
    let json = eval(
        r#"
class Container { value = 42 }
class Holder { selected: *Container | String }
local make = () -> new Holder {}
result = make.apply()
"#,
    );
    assert_eq!(json["result"]["selected"]["value"], 42);
}

#[test]
fn lambda_object_keeps_quoted_default_type_binding() {
    let json = eval(
        r#"
class `Foo-Bar` { value = 42 }
class `My Step` { value = 43 }
class Holder {
  selected: *`Foo-Bar` | String
  spaced: *`My Step` | String
}
local make = () -> new Holder {}
result = make.apply()
"#,
    );
    assert_eq!(json["result"]["selected"]["value"], 42);
    assert_eq!(json["result"]["spaced"]["value"], 43);
}

#[test]
fn lambda_let_and_for_bindings_shadow_captured_names() {
    // The capture collector ignores shadowing, so it also captures the outer
    // `x`; the inner `let` and `for` bindings must still win.
    let json = eval(
        r#"
local x = 1
local f = (n) -> let (x = n + 10) x
local g = (xs) -> new Listing { for (x in xs) { x * 2 } }
local h = (n) -> let (y = n) y + x
a = f.apply(1)
b = g.apply(List(1, 2))
c = h.apply(5)
"#,
    );
    assert_eq!(json["a"], 11);
    assert_eq!(json["b"], serde_json::json!([2, 4]));
    assert_eq!(json["c"], 6);
}

#[test]
fn lambda_constrained_check_keeps_quoted_class_with_comma() {
    let json = eval(
        r#"
class `Foo,Bar` { value = 1 }
class Other { value = 2 }
local check = (v) -> v is `Foo,Bar`(true)
yes = check.apply(new `Foo,Bar` {})
no = check.apply(new Other {})
"#,
    );
    assert_eq!(json["yes"], true);
    assert_eq!(json["no"], false);
}

#[test]
fn lambda_constrained_check_keeps_quoted_generic_class() {
    let json = eval(
        r#"
class `Box,Pair` { value = 1 }
class Other { value = 2 }
local check = (v) -> v is `Box,Pair`(true)
yes = check.apply(new `Box,Pair` {})
no = check.apply(new Other {})
"#,
    );
    assert_eq!(json["yes"], true);
    assert_eq!(json["no"], false);
}

// ============================================================
// IntSeq
// ============================================================

#[test]
fn int_seq_for_generator() {
    let json = eval(
        r#"
squares {
  for (i in IntSeq(1, 5)) {
    i * i
  }
}
byKey {
  for (idx, i in IntSeq(3, 4)) {
    ["k\(i)"] = idx
  }
}
"#,
    );
    assert_eq!(json["squares"], serde_json::json!([1, 4, 9, 16, 25]));
    assert_eq!(json["byKey"], serde_json::json!({"k3": 0, "k4": 1}));
}

#[test]
fn int_seq_to_list_and_properties() {
    let json = eval(
        r#"
list = IntSeq(1, 5).toList()
single = IntSeq(7, 7).toList()
negative = IntSeq(-2, 2).toList()
first = IntSeq(3, 9).first
last = IntSeq(3, 9).last
empty = IntSeq(1, 0).isEmpty
notEmpty = IntSeq(1, 1).isEmpty
len = IntSeq(1, 10).length
local seq = IntSeq(0, 3)
viaLocal = seq.toList()
"#,
    );
    assert_eq!(json["list"], serde_json::json!([1, 2, 3, 4, 5]));
    assert_eq!(json["single"], serde_json::json!([7]));
    assert_eq!(json["negative"], serde_json::json!([-2, -1, 0, 1, 2]));
    assert_eq!(json["first"], 3);
    assert_eq!(json["last"], 9);
    assert_eq!(json["empty"], true);
    assert_eq!(json["notEmpty"], false);
    assert_eq!(json["len"], 10);
    assert_eq!(json["viaLocal"], serde_json::json!([0, 1, 2, 3]));
}

#[test]
fn int_seq_step() {
    let json = eval(
        r#"
evens = IntSeq(0, 10).step(2).toList()
uneven = IntSeq(1, 10).step(3).toList()
down = IntSeq(5, 1).step(-1).toList()
downBy2 = IntSeq(10, 1).step(-2).toList()
wrongWayUp = IntSeq(1, 5).step(-1).toList()
looped {
  for (i in IntSeq(10, 0).step(-5)) {
    i
  }
}
"#,
    );
    assert_eq!(json["evens"], serde_json::json!([0, 2, 4, 6, 8, 10]));
    assert_eq!(json["uneven"], serde_json::json!([1, 4, 7, 10]));
    assert_eq!(json["down"], serde_json::json!([5, 4, 3, 2, 1]));
    assert_eq!(json["downBy2"], serde_json::json!([10, 8, 6, 4, 2]));
    assert_eq!(json["wrongWayUp"], serde_json::json!([]));
    assert_eq!(json["looped"], serde_json::json!([10, 5, 0]));
}

#[test]
fn int_seq_empty_range() {
    let json = eval(
        r#"
list = IntSeq(5, 1).toList()
isEmpty = IntSeq(5, 1).isEmpty
generated {
  for (i in IntSeq(5, 1)) {
    i
  }
}
"#,
    );
    assert_eq!(json["list"], serde_json::json!([]));
    assert_eq!(json["isEmpty"], true);
    assert_eq!(json["generated"], serde_json::json!([]));
}

#[test]
fn int_seq_map_and_fold() {
    let json = eval(
        r#"
doubled = IntSeq(1, 4).map((i) -> i * 2)
sum = IntSeq(1, 100).fold(0, (acc, i) -> acc + i)
stepSum = IntSeq(0, 10).step(5).fold(0, (acc, i) -> acc + i)
names = IntSeq(1, 3).map((i) -> "host\(i)").join(",")
"#,
    );
    assert_eq!(json["doubled"], serde_json::json!([2, 4, 6, 8]));
    assert_eq!(json["sum"], 5050);
    assert_eq!(json["stepSum"], 15);
    assert_eq!(json["names"], "host1,host2,host3");
}

#[test]
fn int_seq_errors() {
    let err = eval_fails("x = IntSeq(1, 5).step(0).toList()");
    assert!(err.contains("step must not be 0"), "{err}");
    let err = eval_fails("x = IntSeq(0, 9223372036854775807).toList()");
    assert!(err.contains("supported maximum"), "{err}");
    let err = eval_fails(r#"x = IntSeq(1, "5")"#);
    assert!(err.contains("Int arguments"), "{err}");
    let err = eval_fails("x = IntSeq(1)");
    assert!(err.contains("expects 2 arguments"), "{err}");
}

#[test]
fn int_seq_user_binding_shadows_builtin() {
    let json = eval(
        r#"
x = ((IntSeq) -> IntSeq(1, 2))((a, b) -> a + b)
local IntSeq = (a, b) -> a * b
y = IntSeq(3, 4)
"#,
    );
    assert_eq!(json["x"], 3);
    assert_eq!(json["y"], 12);
}

#[test]
fn int_seq_boundaries() {
    let json = eval(
        r#"
atCap = IntSeq(1, 1000000).length
nearMax = IntSeq(9223372036854775806, 9223372036854775807).toList()
downFromMax = IntSeq(9223372036854775807, 9223372036854775806).step(-1).toList()
"#,
    );
    assert_eq!(json["atCap"], 1_000_000);
    assert_eq!(
        json["nearMax"],
        serde_json::json!([9223372036854775806i64, 9223372036854775807i64])
    );
    assert_eq!(
        json["downFromMax"],
        serde_json::json!([9223372036854775807i64, 9223372036854775806i64])
    );
    let err = eval_fails("x = IntSeq(1, 1000001).length");
    assert!(err.contains("supported maximum"), "{err}");
}

#[test]
fn int_seq_step_argument_errors() {
    let err = eval_fails("x = IntSeq(1, 5).step()");
    assert!(err.contains("exactly one argument"), "{err}");
    let err = eval_fails("x = IntSeq(1, 5).step(1, 2)");
    assert!(err.contains("exactly one argument"), "{err}");
    let err = eval_fails(r#"x = IntSeq(1, 5).step("2")"#);
    assert!(err.contains("expects an Int"), "{err}");
}

#[test]
fn lambda_sees_type_aliases_of_enclosing_body() {
    let json = eval(
        r#"
length = 1
result {
  typealias String = Int
  f = (x) -> x is String
  g = (x) -> x is String(this == length)
  h = (x) -> x as String
  a = f.apply(1)
  b = g.apply(1)
  c = h.apply(1)
  d = 1 is String
}
"#,
    );
    assert_eq!(json["result"]["a"], true);
    assert_eq!(json["result"]["b"], true);
    assert_eq!(json["result"]["c"], 1);
    assert_eq!(json["result"]["d"], true);
}

#[test]
fn lambda_sees_module_type_aliases() {
    let json = eval(
        r#"
typealias Small = Int(this < 10)
f = (x) -> x is Small
a = f.apply(1)
b = f.apply(20)
"#,
    );
    assert_eq!(json["a"], true);
    assert_eq!(json["b"], false);
}

#[test]
fn lambda_constraint_on_aliased_int_reads_enclosing_binding() {
    // With `String = Int`, `length` in the constraint is not a member of the
    // checked value, so it names the enclosing local, failures included.
    let message = eval_fails(
        r#"
result {
  typealias String = Int
  local length = throw("length failed")
  local g = (x) -> x is String(this == length)
  value = g.apply(1)
}
"#,
    );
    assert!(message.contains("length failed"), "{message}");
}

#[test]
fn lambda_resolves_alias_chains_and_constraints() {
    let json = eval(
        r#"
typealias Small = Int(this < 10)
typealias Tiny = Small
result {
  typealias Even = Tiny(this % 2 == 0)
  local inner = (x) -> (y) -> y is Even
  f = (x) -> x is Even
  a = f.apply(4)
  b = f.apply(5)
  c = f.apply(12)
  d = inner.apply(0).apply(4)
  e = f.apply(null)
}
"#,
    );
    assert_eq!(json["result"]["a"], true);
    assert_eq!(json["result"]["b"], false);
    assert_eq!(json["result"]["c"], false);
    assert_eq!(json["result"]["d"], true);
    assert_eq!(json["result"]["e"], false);
}

#[test]
fn lambda_keeps_alias_redeclared_in_nested_body() {
    let json = eval(
        r#"
result {
  typealias T = Int
  f = (x) -> new Dynamic {
    typealias T = String
    inner = x is T
  }.inner
  g = (x) -> new Dynamic { outer = x is T }.outer
  a = f.apply("s")
  b = f.apply(1)
  c = g.apply(1)
}
"#,
    );
    assert_eq!(json["result"]["a"], true);
    assert_eq!(json["result"]["b"], false);
    assert_eq!(json["result"]["c"], true);
}

#[test]
fn lambda_with_self_referential_alias_constraint_is_created() {
    let json = eval(
        r#"
typealias A = Int(this == 0 || this is A)
f = (x) -> x is A
a = f.apply(0)
"#,
    );
    assert_eq!(json["a"], true);
}

#[test]
fn lambda_resolves_alias_chain_in_its_defining_scope() {
    // `A` means the enclosing `B`, even where a body inside the lambda
    // declares a `B` of its own.
    let json = eval(
        r#"
result {
  typealias B = Int
  typealias A = B
  f = (x) -> new Dynamic {
    typealias B = String
    inner = x is A
  }.inner
  a = f.apply(1)
  b = f.apply("s")
}
"#,
    );
    assert_eq!(json["result"]["a"], true);
    assert_eq!(json["result"]["b"], false);
}
