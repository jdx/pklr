//! A module's value holds amended copies of itself, so its members share
//! sub-objects. Walking them as a tree is exponential: these must finish.

use std::sync::mpsc;
use std::time::Duration;

/// Evaluate `main.pkl`, which extends `base.pkl`, on a thread that is given
/// a deadline, so a runaway evaluation fails the test instead of hanging it.
fn eval_extending(base: &str, main: &str) -> serde_json::Value {
    let dir = std::env::temp_dir().join(format!("pklr-shared-members-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("base.pkl"), base).unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(&path, main).unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(pklr::eval_to_json(&path));
    });
    let result = receive.recv_timeout(Duration::from_secs(30));
    let _ = std::fs::remove_dir_all(&dir);
    result
        .expect("evaluation did not finish")
        .expect("evaluation failed")
}

#[test]
fn caught_error_in_a_module_that_amends_this_finishes() {
    let base = r#"open module base
import "pkl:test"

x: Int = 1
res2 = this
res3 = (this) { x = 2 }
res4 = (this) { x = 3 }
res5 = List(this, (this) { x = 4 })

class Bad { r: Int = "abc" }

caught = test.catch(() -> new Bad {}.r)

output {
  value = new Dynamic { result = "ok" }
}
"#;
    let result = eval_extending(base, "extends \"base.pkl\"\n");
    assert_eq!(result, serde_json::json!({"result": "ok"}));
}
