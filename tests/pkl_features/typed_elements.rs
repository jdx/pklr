use super::*;

// Element, key and value types of `Listing`, `Mapping`, `List`, `Set` and `Map`, and
// the parameter and result types of functions, are checked. Each expectation
// below was compared with `pkl eval` 0.32.2: `None` is an error, `Some(json)`
// the result (without null members, as pkl renders it).

const CASES: &[(&str, Option<&str>)] = &[
    (
        r####"function f(x: Listing<Int>): Int = 1; y = f(new Dynamic { 1 })"####,
        None,
    ),
    (
        r####"function f(x: Listing<Int>): Int = 1; y = f(new Listing { 1 })"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x: Listing<Int>): Int = 1; y = f(List(1))"####,
        None,
    ),
    (r####"function f(x: Int): Int = x; y = f("a")"####, None),
    (r####"function f(x: Int): Int = "a"; y = f(1)"####, None),
    (
        r####"function f(x: Int): String = "a"; y = f(1)"####,
        Some(r####"{"y":"a"}"####),
    ),
    (
        r####"function f(x: Int?): Int = 1; y = f(null)"####,
        Some(r####"{"y":1}"####),
    ),
    (r####"function f(x: Int?): Int = 1; y = f("a")"####, None),
    (
        r####"function f(x: String): Int = 1; y = f(new Dynamic { 1 })"####,
        None,
    ),
    (
        r####"function f(x: Dynamic): Int = 1; y = f(new Dynamic { 1 })"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = 1; y = f(new Mapping { [1] = 1 })"####,
        None,
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = 1; y = f(new Mapping { ["a"] = 1 })"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(): Listing<Int> = new Listing { "a" }; y = f()"####,
        None,
    ),
    (
        r####"function f(): Listing<Int> = new Listing { 1 }; y = f()"####,
        Some(r####"{"y":[1]}"####),
    ),
    (
        r####"function f(): Listing<Int> = new Dynamic { 1 }; y = f()"####,
        None,
    ),
    (
        r####"function f(): Mapping<String, Int> = new Mapping { ["a"] = "b" }; y = f()"####,
        None,
    ),
    (
        r####"function f(x: Int(this > 0)): Int = x; y = f(0)"####,
        None,
    ),
    (
        r####"function f(x: Int(this > 0)): Int = x; y = f(1)"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x): Int = 1; y = f("a")"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x: Any): Int = 1; y = f("a")"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x: List<Int>): Int = 1; y = f(List("a"))"####,
        None,
    ),
    (
        r####"function f(x: List<Int>): Int = 1; y = f(List(1))"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x: Set<Int>): Int = 1; y = f(Set("a"))"####,
        None,
    ),
    (
        r####"function f(x: Map<String,Int>): Int = 1; y = f(Map("a","b"))"####,
        None,
    ),
    (
        r####"function f(x: Listing<Listing<Int>>): Int = 1; y = f(new Listing { new Listing { 1 } })"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x: Listing<Int?>): Int = 1; y = f(new Listing { null; 1 })"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x: Listing<Int|String>): Int = 1; y = f(new Listing { 1; "a" })"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"function f(x: Listing<Int>(length == 1)): Int = 1; y = f(new Listing { 1; 2 })"####,
        None,
    ),
    (
        r####"function f(x: Listing<Int(this > 0)>): Int = 1; y = f(new Listing { 1; 2 })"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"typealias L = Listing<Int>; function f(x: L): Int = 1; y = f(new Listing { 1 })"####,
        Some(r####"{"y":1}"####),
    ),
    (
        r####"typealias L = Listing<Int>; function f(x: L): Int = 1; y = f(new Dynamic { 1 })"####,
        None,
    ),
    (
        r####"typealias E<T> = Listing<T>; function f(x: E<Int>): Int = 1; y = f(new Dynamic { 1 })"####,
        None,
    ),
    (r####"a: Listing<Int> = new Listing { "a" }"####, None),
    (
        r####"a: Listing<Int> = new Listing { 1 }"####,
        Some(r####"{"a":[1]}"####),
    ),
    (r####"a: Listing<Int> = new Dynamic { 1 }"####, None),
    (r####"a: Listing<Int>? = new Listing { "a" }"####, None),
    (r####"a: Listing<Int>? = null"####, Some(r####"{}"####)),
    (
        r####"a: Listing<Listing<Int>> = new Listing { new Listing { "a" } }"####,
        None,
    ),
    (
        r####"a: Listing<Listing<Int>> = new Listing { new Listing { 1 } }"####,
        Some(r####"{"a":[[1]]}"####),
    ),
    (
        r####"a: Listing<Listing<Int>> = new Listing { new Dynamic { 1 } }"####,
        None,
    ),
    (
        r####"a: Listing<Mapping<String,Int>> = new Listing { new Mapping { ["k"] = "v" } }"####,
        None,
    ),
    (
        r####"a: Listing<Mapping<String,Int>> = new Listing { new Mapping { ["k"] = 1 } }"####,
        Some(r####"{"a":[{"k":1}]}"####),
    ),
    (
        r####"a: Mapping<String, Int> = new Mapping { ["a"] = "b" }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Int> = new Mapping { [1] = 1 }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Int> = new Mapping { ["a"] = 1 }"####,
        Some(r####"{"a":{"a":1}}"####),
    ),
    (
        r####"a: Mapping<String, Int> = new Mapping { ["a"] = new Dynamic { 1 } }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Listing<Int>> = new Mapping { ["a"] = new Dynamic { 1 } }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Listing<Int>> = new Mapping { ["a"] = new Listing { "x" } }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Listing<Int>> = new Mapping { ["a"] = new Listing { 1 } }"####,
        Some(r####"{"a":{"a":[1]}}"####),
    ),
    (
        r####"a: Mapping<String, Int?> = new Mapping { ["a"] = null }"####,
        Some(r####"{"a":{}}"####),
    ),
    (
        r####"a: Mapping<String, Int> = new Mapping { ["a"] = null }"####,
        None,
    ),
    (
        r####"a: Mapping<String|Int, Int> = new Mapping { [true] = 1 }"####,
        None,
    ),
    (
        r####"a: Mapping<Int(this > 0), Int> = new Mapping { [0] = 1 }"####,
        None,
    ),
    (
        r####"a: Mapping<Int, Int(this > 0)> = new Mapping { [1] = 0 }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Int> = new Mapping { ["a"] = 1 } "####,
        Some(r####"{"a":{"a":1}}"####),
    ),
    (
        r####"a: Mapping<String, Int> = new Mapping { for (i in List(1,2)) { ["k\(i)"] = i } }"####,
        Some(r####"{"a":{"k1":1,"k2":2}}"####),
    ),
    (
        r####"a: Mapping<String, Int> = new Mapping { for (i in List(1,2)) { ["k\(i)"] = "x" } }"####,
        None,
    ),
    (
        r####"typealias L = Listing<Int>; a: L = new Listing { "a" }"####,
        None,
    ),
    (
        r####"typealias L = Listing<Int>; a: L? = new Listing { "a" }"####,
        None,
    ),
    (
        r####"typealias L<T> = Listing<T>; a: L<Int> = new Listing { 1 }"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"typealias M = Mapping<String,Int>; a: M = new Mapping { ["a"] = "b" }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { "a" } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { 1 } }"####,
        Some(r####"{"a":{"x":[1]}}"####),
    ),
    (
        r####"class C { x: Listing<Int> = new Listing { "a" } }; a = new C {}"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x = new Listing { "a" } }"####,
        None,
    ),
    (
        r####"class C { x: Mapping<String,Int> }; a = new C { x { ["a"] = "b" } }"####,
        None,
    ),
    (
        r####"class C { x: Mapping<String,Int> }; a = new C { x { ["a"] = 1 } }"####,
        Some(r####"{"a":{"x":{"a":1}}}"####),
    ),
    (
        r####"class C { x: Mapping<String,Int> = new Mapping { ["a"] = 1 } }; a = new C { x { ["a"] = "b" } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> = new Listing { 1 } }; a = new C { x { "b" } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> = new Listing { 1 } }; a = new C { x { 2 } }"####,
        Some(r####"{"a":{"x":[1,2]}}"####),
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { new Dynamic { 1 } } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<String>? = null }; a = new C { x { 1 } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<String>? = null }; a = new C { x { "a" } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int|String> }; a = new C { x { 1; "a"; true } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { for (i in List("a")) { i } } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { ...List("a") } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { ...List(1) } }"####,
        Some(r####"{"a":{"x":[1]}}"####),
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { when (true) { "a" } } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { when (false) { "a" } } }"####,
        Some(r####"{"a":{"x":[]}}"####),
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { "a" } }; b = a.x.length"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { 1 } }; b = a.x[0]"####,
        Some(r####"{"a":{"x":[1]},"b":1}"####),
    ),
    (
        r####"class C { x: Listing<Int> }; a = new C { x { "a" } }; b = 1"####,
        None,
    ),
    (r####"a: Listing<Int> = new { "a" }"####, None),
    (
        r####"a: Listing<Int> = new { 1 }"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"a: Mapping<String,Int> = new { ["a"] = "b" }"####,
        None,
    ),
    (
        r####"a: Mapping<String,Int> = new { ["a"] = 1 }"####,
        Some(r####"{"a":{"a":1}}"####),
    ),
    (
        r####"a: Listing<Int> = (new Listing { "a" }) // lazy"####,
        None,
    ),
    (
        r####"a: Listing<Int> = new Listing { 1 }; b = a.length"####,
        Some(r####"{"a":[1],"b":1}"####),
    ),
    (
        r####"a: Listing<Int> = new Listing { "a" }; b = a.length"####,
        None,
    ),
    (
        r####"a: Listing<Int> = new Listing { "a" }; b = a.isEmpty"####,
        None,
    ),
    (
        r####"a: Listing<Int> = new Listing { "a" }; b = a.toList()"####,
        None,
    ),
    (
        r####"a: Listing<Int> = new Listing { "a" }; b = a[0]"####,
        None,
    ),
    (
        r####"a: Listing<Int> = new Listing { 1; "a" }; b = a[0]"####,
        None,
    ),
    (
        r####"a: Listing<Int> = new Listing { 1; "a" }; b = a[1]"####,
        None,
    ),
    (
        r####"a: Listing<Int> = new Listing { "a" }; b = 1 + 1"####,
        None,
    ),
    (
        r####"local l: Listing<Int> = new Listing { "a" }; b = 1"####,
        Some(r####"{"b":1}"####),
    ),
    (
        r####"local l: Listing<Int> = new Listing { "a" }; b = l.length"####,
        Some(r####"{"b":1}"####),
    ),
    (
        r####"a: Listing<Int> = new Listing { "a" }; output { renderer = new JsonRenderer {} }"####,
        None,
    ),
    (
        r####"a: Listing<Int(this > 0)> = new Listing { 0 }"####,
        None,
    ),
    (
        r####"a: Listing<Int(this > 0)> = new Listing { 1 }"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"a: Listing<Int>(length == 2) = new Listing { 1 }"####,
        None,
    ),
    (
        r####"a: Listing<Int>(length == 2) = new Listing { 1; 2 }"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (
        r####"a: Listing<Int>(length == 2) = new Listing { "a" }"####,
        None,
    ),
    (
        r####"a: Listing<Int>(length == 2) = new Listing { "a"; "b" }"####,
        None,
    ),
    (
        r####"a: Listing<Int|String> = new Listing { 1; "a" }"####,
        Some(r####"{"a":[1,"a"]}"####),
    ),
    (
        r####"a: Listing<Int|String> = new Listing { 1; true }"####,
        None,
    ),
    (
        r####"a: Listing<"a"|"b"> = new Listing { "a"; "c" }"####,
        None,
    ),
    (
        r####"a: Listing<"a"|"b"> = new Listing { "a"; "b" }"####,
        Some(r####"{"a":["a","b"]}"####),
    ),
    (
        r####"a: Listing<Number> = new Listing { 1; 2.0; "x" }"####,
        None,
    ),
    (
        r####"a: Listing<Number> = new Listing { 1; 2.0 }"####,
        Some(r####"{"a":[1,2.0]}"####),
    ),
    (
        r####"a: Listing<Any> = new Listing { 1; "x" }"####,
        Some(r####"{"a":[1,"x"]}"####),
    ),
    (
        r####"a: Listing<unknown> = new Listing { 1; "x" }"####,
        Some(r####"{"a":[1,"x"]}"####),
    ),
    (
        r####"a: Listing = new Listing { 1; "x" }"####,
        Some(r####"{"a":[1,"x"]}"####),
    ),
    (
        r####"a: Listing<Dynamic> = new Listing { new Dynamic { 1 } }"####,
        Some(r####"{"a":[[1]]}"####),
    ),
    (r####"a: Listing<Dynamic> = new Listing { 1 }"####, None),
    (r####"a: Listing<Object> = new Listing { 1 }"####, None),
    (
        r####"a: Listing<Object> = new Listing { new Dynamic { 1 } }"####,
        Some(r####"{"a":[[1]]}"####),
    ),
    (
        r####"a: Listing<Listing<Int>> = new Listing { new Listing { 1 }; new Listing { "x" } }"####,
        None,
    ),
    (
        r####"a: Listing<Listing<Listing<Int>>> = new Listing { new Listing { new Listing { "x" } } }"####,
        None,
    ),
    (
        r####"a: Listing<Listing<Listing<Int>>> = new Listing { new Listing { new Listing { 1 } } }"####,
        Some(r####"{"a":[[[1]]]}"####),
    ),
    (
        r####"a: Listing<Listing<Int>?> = new Listing { null; new Listing { "x" } }"####,
        None,
    ),
    (
        r####"a: Listing<Listing<Int>?> = new Listing { null; new Listing { 1 } }"####,
        Some(r####"{"a":[null,[1]]}"####),
    ),
    (r####"a: Listing<Int>? = new Listing { 1; "x" }"####, None),
    (
        r####"a: Listing<String>|Listing<Int> = new Listing { 1 }"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"a: Listing<String>|Listing<Int> = new Listing { true }"####,
        None,
    ),
    (r####"a: Listing<Int>|Int = new Listing { "x" }"####, None),
    (r####"a: Listing<Int>|Int = 1"####, Some(r####"{"a":1}"####)),
    (r####"a: List<Int> = List("a")"####, None),
    (
        r####"a: List<Listing<Int>> = List(new Listing { "a" })"####,
        None,
    ),
    (
        r####"a: List<Listing<Int>> = List(new Listing { 1 })"####,
        Some(r####"{"a":[[1]]}"####),
    ),
    (
        r####"a: Set<Listing<Int>> = Set(new Listing { "a" })"####,
        None,
    ),
    (
        r####"a: Map<String, Listing<Int>> = Map("a", new Listing { "x" })"####,
        None,
    ),
    (
        r####"a: Map<String, Listing<Int>> = Map("a", new Listing { 1 })"####,
        Some(r####"{"a":{"a":[1]}}"####),
    ),
    (r####"a: Pair<Int, Int> = Pair(1, "x")"####, None),
    (
        r####"a: Collection<Listing<Int>> = List(new Listing { "x" })"####,
        None,
    ),
    (r####"a: Listing<Int> = List("a").toListing()"####, None),
    (
        r####"a: Mapping<String, Int> = Map("a", "b").toMapping()"####,
        None,
    ),
    (
        r####"a: Mapping<String, Int> = Map("a", 1).toMapping()"####,
        Some(r####"{"a":{"a":1}}"####),
    ),
    (
        r####"a: Listing<Int> = new Listing { 1 }.toList()"####,
        None,
    ),
    (
        r####"a = new Listing<Int> { 1 }"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"local l: Listing<Int> = new Listing { 1; "a" }; a = l.length"####,
        Some(r####"{"a":2}"####),
    ),
    (
        r####"local l: Listing<Int> = new Listing { 1; "a" }; a = l[0]"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"local l: Listing<Int> = new Listing { 1; "a" }; a = l is Listing<Int>"####,
        Some(r####"{"a":false}"####),
    ),
    (
        r####"a = new Listing { 1; "a" } is Listing<Int>"####,
        Some(r####"{"a":false}"####),
    ),
    (
        r####"a = new Listing { 1; 2 } is Listing<Int>"####,
        Some(r####"{"a":true}"####),
    ),
    (
        r####"a = new Mapping { ["a"] = 1 } is Mapping<String, Int>"####,
        Some(r####"{"a":true}"####),
    ),
    (
        r####"a = new Mapping { ["a"] = "b" } is Mapping<String, Int>"####,
        Some(r####"{"a":false}"####),
    ),
    (r####"a = new Listing { 1; "a" } as Listing<Int>"####, None),
    (
        r####"a = new Listing { 1; "a" } as Listing<Int>; b = 1"####,
        None,
    ),
    (
        r####"a = (new Listing { 1; "a" } as Listing<Int>).toList()"####,
        None,
    ),
    (
        r####"a = new Mapping { ["a"] = "b" } as Mapping<String, Int>"####,
        None,
    ),
    (r####"x = 1"####, Some(r####"{"x":1}"####)),
    (
        r####"class C { x: Listing<Int> }; a: C = new C { x = new Listing { "a" } }"####,
        None,
    ),
    (
        r####"class C { x: Listing<Int> }; a: Listing<C> = new Listing { new C { x = new Listing { "a" } } }"####,
        None,
    ),
    (
        r####"class C { x: Int }; a: Listing<C> = new Listing { new C { x = "a" } }"####,
        None,
    ),
    (
        r####"class C { x: Int }; a: Listing<C> = new Listing { new C { x = 1 } }"####,
        Some(r####"{"a":[{"x":1}]}"####),
    ),
    (
        r####"class C { x: Int }; a: Listing<C> = new Listing { new Dynamic { x = 1 } }"####,
        None,
    ),
    (
        r####"class C { x: Int }; a: Listing<C> = new Listing { new Listing { x = 1 } }"####,
        None,
    ),
    (
        r####"class C { x: Int }; a: Mapping<String, C> = new Mapping { ["k"] = new C { x = "a" } }"####,
        None,
    ),
    (
        r####"class C { x: Int }; a: Mapping<String, C> = new Mapping { ["k"] = new C { x = 1 } }"####,
        Some(r####"{"a":{"k":{"x":1}}}"####),
    ),
    (
        r####"class C { x: Int }; function f(c: C): Int = c.x; a = f(new C { x = "a" })"####,
        None,
    ),
    (
        r####"class C { x: Int }; function f(c: C): Int = c.x; a = f(new C { x = 1 })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"class C { x: Int }; function f(c: C): Int = c.x; a = f(new Dynamic { x = 1 })"####,
        None,
    ),
    (
        r####"class C { x: Int }; class D extends C {}; function f(c: C): Int = c.x; a = f(new D { x = 1 })"####,
        None,
    ),
    (
        r####"class C { function g(x: Listing<Int>): Int = 1 }; a = new C {}.g(new Listing { 1 })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"class C { function g(x: Int): Int = 1 }; a = new C {}.g("a")"####,
        None,
    ),
    (
        r####"class C { function g(x: Int): String = 1 }; a = new C {}.g(1)"####,
        None,
    ),
    (
        r####"class C { function g(x: Int): Int = x }; a = new C {}.g(1)"####,
        Some(r####"{"a":1}"####),
    ),
    (r####"local f = (x: Int) -> x; a = f("a")"####, None),
    (
        r####"local f = (x: Listing<Int>) -> 1; a = f(new Listing { "a" })"####,
        None,
    ),
    (
        r####"local f = (x: Listing<Int>) -> 1; a = f(new Dynamic { 1 })"####,
        None,
    ),
    (
        r####"local f = (x: Mapping<String,Int>) -> 1; a = f(new Mapping { ["a"] = "b" })"####,
        None,
    ),
    (
        r####"a = List(1, 2).map((x: Int) -> x)"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (r####"a = List(1, "a").map((x: Int) -> x)"####, None),
    (r####"a = List(1, 2).map((x: String) -> x)"####, None),
    (
        r####"a = List(1, 2).map((x: Int) -> x.toString())"####,
        Some(r####"{"a":["1","2"]}"####),
    ),
    (
        r####"a = new Listing { 1; 2 }.toList().map((x: String) -> x)"####,
        None,
    ),
    (
        r####"a = new Listing { 1; 2 }.map((x: String) -> x)"####,
        None,
    ),
    (
        r####"a = List(1,2).fold(0, (acc: Int, x: Int) -> acc + x)"####,
        Some(r####"{"a":3}"####),
    ),
    (
        r####"a = List(1,2).fold(0, (acc: String, x: Int) -> acc + x)"####,
        None,
    ),
    (
        r####"function f(x: Int, y: String): Int = x; a = f(1, 2)"####,
        None,
    ),
    (
        r####"function f(x: Int, y: String): Int = x; a = f(1, "b")"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Int, y: String = "d"): Int = x; a = f(1)"####,
        None,
    ),
    (
        r####"function f(x: Int, y: String = 2): Int = x; a = f(1)"####,
        None,
    ),
    (
        r####"function f(x: Int(isPositive)): Int = x; a = f(-1)"####,
        None,
    ),
    (
        r####"function f(x: Int(isPositive)): Int = x; a = f(1)"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Int): Int(isPositive) = x; a = f(-1)"####,
        None,
    ),
    (
        r####"function f(x: Int): Int(isPositive) = x; a = f(1)"####,
        Some(r####"{"a":1}"####),
    ),
    (r####"function f(x: Number): Int = x; a = f(1.5)"####, None),
    (
        r####"function f(x: Number): Int = x; a = f(1)"####,
        Some(r####"{"a":1}"####),
    ),
    (r####"function f(x: Float): Float = x; a = f(1)"####, None),
    (
        r####"function f(x: Float): Float = x; a = f(1.0)"####,
        Some(r####"{"a":1.0}"####),
    ),
    (
        r####"function f(x: String): Int = x.length; a = f(null)"####,
        None,
    ),
    (
        r####"function f(x: String?): Int = 1; a = f(null)"####,
        Some(r####"{"a":1}"####),
    ),
    (r####"function f(x: Int): Int = 1; a = f(null)"####, None),
    (
        r####"function f(x: Listing<Int>?): Int = 1; a = f(null)"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Listing<Int>): Int = 1; a = f(null)"####,
        None,
    ),
    (
        r####"function f(x: Listing<Int>): Int = 1; a = f(1)"####,
        None,
    ),
    (
        r####"function f(x: Listing<String>): Listing<Int> = new Listing { 1 }; a = f(new Listing { "a" })"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"function f(x: Listing<Int>): Listing<Int> = x; a = f(new Listing { 1 })"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"function f(x: Listing<Int>): Listing<Int> = x; a = f(new Listing { "a" })"####,
        None,
    ),
    (
        r####"function f(x: Listing<Int>): Listing<String> = x; a = f(new Listing { 1 })"####,
        None,
    ),
    (
        r####"function f(x: Listing<Int>): Int = x.length; a = f(new Listing { 1 })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Listing<Int>): Int = x[0]; a = f(new Listing { "a" })"####,
        None,
    ),
    (
        r####"function f(x: Listing<Int>): Int = 1; a = f(new Listing { when (false) { "a" } })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"class C { x: Listing<Int> }; function f(c: C): Int = 1; a = f(new C { x { 1 } })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = x.length; a = f(new Mapping { ["a"] = 1 })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = x["a"]; a = f(new Mapping { ["a"] = "b" })"####,
        None,
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = x.keys.length; a = f(new Mapping { [1] = 1 })"####,
        None,
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = 1; a = f(new Mapping { ["a"] = 1; [2] = 2 })"####,
        None,
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = 1; a = f(new Dynamic { ["a"] = 1 })"####,
        None,
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = 1; a = f(new Dynamic { ["a"] = "b" })"####,
        None,
    ),
    (
        r####"function f(x: Mapping<String, Int>): Int = 1; a = f(new Mapping { })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Mapping<String, Listing<Int>>): Int = 1; a = f(new Mapping { ["a"] = new Listing { 1 } })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"class D { path: String?; line: Int? }; a: Listing<D> = new Listing { new Dynamic { path = "a" } }"####,
        None,
    ),
    (
        r####"class D { path: String?; line: Int? }; a: Mapping<String, D> = new Mapping { ["k"] = new Dynamic { path = "a" } }"####,
        None,
    ),
    (
        r####"class D { path: String?; line: Int? }; function f(d: D): String = "x"; a = f(new Dynamic { path = "a" })"####,
        None,
    ),
    (
        r####"class D { path: String?; line: Int? }; function f(d: D): String = "x"; a = f(new D { path = "a" })"####,
        Some(r####"{"a":"x"}"####),
    ),
    (r####"function f(x: Int): Int = x; a = f(1.5)"####, None),
    (
        r####"function f(x: Number): Number = x; a = f(1.5)"####,
        Some(r####"{"a":1.5}"####),
    ),
    (
        r####"function f(x: Boolean): Boolean = x; a = f(1)"####,
        None,
    ),
    (
        r####"function f(x: String|Int): Int = 1; a = f(true)"####,
        None,
    ),
    (
        r####"function f(x: String|Int): Int = 1; a = f(1)"####,
        Some(r####"{"a":1}"####),
    ),
    (r####"function f(x: "a"|"b"): Int = 1; a = f("c")"####, None),
    (
        r####"function f(x: "a"|"b"): Int = 1; a = f("a")"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Duration): Int = 1; a = f(5.s)"####,
        Some(r####"{"a":1}"####),
    ),
    (r####"function f(x: Duration): Int = 1; a = f(5)"####, None),
    (
        r####"function f(x: List<Int>): Int = 1; a = f(Set(1))"####,
        None,
    ),
    (
        r####"function f(x: Collection<Int>): Int = 1; a = f(Set(1))"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Collection<Int>): Int = 1; a = f(Set("a"))"####,
        None,
    ),
    (
        r####"function f(x: Set<Int>): Int = 1; a = f(Set(1))"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Map<String, Int>): Int = 1; a = f(Map("a", 1))"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Map<String, Int>): Int = 1; a = f(Map(1, 1))"####,
        None,
    ),
    (
        r####"function f(x: Map<String, Int>): Int = 1; a = f(Map("a", "b"))"####,
        None,
    ),
    (r####"a: Map<String, Int> = Map("a", "b")"####, None),
    (r####"a: Map<String, Int> = Map(1, 1)"####, None),
    (
        r####"a: Map<String, Int> = Map("a", 1)"####,
        Some(r####"{"a":{"a":1}}"####),
    ),
    (r####"a: Set<Int> = Set(1, "a")"####, None),
    (
        r####"a: Set<Int> = Set(1, 2)"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (r####"a: Collection<Int> = List(1, "a")"####, None),
    (
        r####"a: List<List<Int>> = List(List(1), List("a"))"####,
        None,
    ),
    (
        r####"a: List<List<Int>> = List(List(1), List(2))"####,
        Some(r####"{"a":[[1],[2]]}"####),
    ),
    (
        r####"a: List<Int?> = List(1, null)"####,
        Some(r####"{"a":[1,null]}"####),
    ),
    (r####"a: List<Int?> = List(1, "a")"####, None),
    (r####"a: List<Int|String> = List(1, "a", true)"####, None),
    (r####"a: List<Int(this > 0)> = List(1, -1)"####, None),
    (
        r####"a: List<Int(this > 0)> = List(1, 2)"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (
        r####"a: Listing<Int>(length == 1) = new Listing { 1 }"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"a: Listing<Int>(length == 1) = new Listing { "a" }"####,
        None,
    ),
    (r####"a: Listing<Int>? = null"####, Some(r####"{}"####)),
    (
        r####"a: Mapping<String, Int>? = null"####,
        Some(r####"{}"####),
    ),
    (
        r####"a: Mapping<String, Int>? = new Mapping { ["a"] = "b" }"####,
        None,
    ),
    (
        r####"typealias L = List<Int>; a: L = List(1, "a")"####,
        None,
    ),
    (
        r####"typealias L = List<Int>; a: L = List(1, 2)"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (
        r####"typealias L<T> = List<T>; a: L<Int> = List(1, 2)"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (
        r####"typealias Foo = List<Foo2>; typealias Foo2 = Int; a: Foo = List(1, 2)"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (
        r####"typealias Foo = List<Foo2>; typealias Foo2 = Int; a: Foo = List(1, "a")"####,
        None,
    ),
    (
        r####"typealias Pair<Foo> = List<Foo>; typealias Foo = Pair<Int>; x: Foo = List(1)"####,
        Some(r####"{"x":[1]}"####),
    ),
    (
        r####"function f(x: Int, y: Int(this > x)): Int = y; a = f(1, 0)"####,
        None,
    ),
    (
        r####"function f(x: Int, y: Int(this > x)): Int = y; a = f(1, 2)"####,
        Some(r####"{"a":2}"####),
    ),
    (
        r####"class C { function g(x: String): String = x }; a = (new C {}).g(1)"####,
        None,
    ),
    (
        r####"class C { function g(x: String): String = x }; a = (new C {}).g("a")"####,
        Some(r####"{"a":"a"}"####),
    ),
    (
        r####"class C { hidden function g(x: Listing<Int>): Int = 1 }; a = (new C {}).g(new Dynamic { 1 })"####,
        None,
    ),
    (
        r####"function f(x: Listing<Int>): Int = x.length; a = f(new Dynamic { 1 })"####,
        None,
    ),
    (
        r####"function f(x: Listing<Int>): Int = x.length; a = f(new Listing { 1 })"####,
        Some(r####"{"a":1}"####),
    ),
    (r####"function f(): Int = "a"; a = f()"####, None),
    (
        r####"function f(): String = "a"; a = f()"####,
        Some(r####"{"a":"a"}"####),
    ),
    (
        r####"function f(): Dynamic = new Dynamic { 1 }; a = f()"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"function f(): Listing<Int> = new Dynamic { 1 }; a = f()"####,
        None,
    ),
    (
        r####"function f(): Listing<Int> = new { 1 }; a = f()"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"function f(): Listing<Int> = new { "a" }; a = f()"####,
        None,
    ),
    (
        r####"function f(): Mapping<String, Int> = new { ["a"] = 1 }; a = f()"####,
        Some(r####"{"a":{"a":1}}"####),
    ),
    (
        r####"function f(): Mapping<String, Int> = new { ["a"] = "b" }; a = f()"####,
        None,
    ),
    (r####"function f(x): Int = x; a = f("a")"####, None),
    (
        r####"function f(x): Int = x; a = f(1)"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Any): Any = x; a = f("a")"####,
        Some(r####"{"a":"a"}"####),
    ),
    (
        r####"function f(x: unknown): unknown = x; a = f("a")"####,
        Some(r####"{"a":"a"}"####),
    ),
    (
        r####"function f(x: Any?): Any? = x; a = f(null)"####,
        Some(r####"{}"####),
    ),
    (
        r####"a = List(1, 2).map((x: Int) -> x + 1)"####,
        Some(r####"{"a":[2,3]}"####),
    ),
    (r####"a = List(1, "a").map((x: Int) -> x)"####, None),
    (r####"a = List(1, 2).filter((x: String) -> true)"####, None),
    (
        r####"a = List(1, 2).map((x: Number) -> x)"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (
        r####"a = List(new Listing { 1 }).map((x: Listing<Int>) -> x.length)"####,
        Some(r####"{"a":[1]}"####),
    ),
    (
        r####"a = List(new Dynamic { 1 }).map((x: Listing<Int>) -> 1)"####,
        None,
    ),
    (
        r####"class C { x: Mapping<String, Listing<Int>> = new { ["a"] { 1 } } }; a = new C {}"####,
        Some(r####"{"a":{"x":{"a":[1]}}}"####),
    ),
    (
        r####"class C { x: Mapping<String, Listing<Int>> = new { ["a"] { "b" } } }; a = new C {}"####,
        None,
    ),
    (
        r####"class C { x: Mapping<String, Listing<Int>> }; a = new C { x { ["a"] { 1 } } }"####,
        Some(r####"{"a":{"x":{"a":[1]}}}"####),
    ),
    (
        r####"class C { x: Mapping<String, Listing<Int>> }; a = new C { x { ["a"] { "b" } } }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Listing<Int>> = new { ["a"] { 1 } }"####,
        Some(r####"{"a":{"a":[1]}}"####),
    ),
    (
        r####"a: Mapping<String, Listing<Int>> = new { ["a"] { "b" } }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Listing<Int>> = new { ["a"] = new Dynamic { 1 } }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Listing<Int>> = new { ["a"] = new Listing { 1 } }"####,
        Some(r####"{"a":{"a":[1]}}"####),
    ),
    (
        r####"a: Mapping<String, Mapping<String, Int>> = new { ["a"] { ["b"] = 1 } }"####,
        Some(r####"{"a":{"a":{"b":1}}}"####),
    ),
    (
        r####"a: Mapping<String, Mapping<String, Int>> = new { ["a"] { ["b"] = "c" } }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Mapping<String, Int>> = new { ["a"] { [1] = 1 } }"####,
        None,
    ),
    (
        r####"a: Mapping<Int, String> = new { ["a"] = "a" }"####,
        None,
    ),
    (
        r####"a: Mapping<Boolean, String> = new { [1] = "a" }"####,
        None,
    ),
    (
        r####"a: Mapping<Duration, String> = new { [1] = "a" }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Any> = new { ["a"] = 1; ["b"] = "c" }"####,
        Some(r####"{"a":{"a":1,"b":"c"}}"####),
    ),
    (
        r####"a: Mapping<String, Int|String> = new { ["a"] = 1; ["b"] = "c" }"####,
        Some(r####"{"a":{"a":1,"b":"c"}}"####),
    ),
    (
        r####"a: Mapping<String, Int|String> = new { ["a"] = 1; ["b"] = true }"####,
        None,
    ),
    (
        r####"a: Listing<Listing<Listing<Int>>> = new { new { new { 1 } } }"####,
        Some(r####"{"a":[[[1]]]}"####),
    ),
    (
        r####"a: Listing<Listing<Listing<Int>>> = new { new { new { "a" } } }"####,
        None,
    ),
    (
        r####"a: Listing<Mapping<String, Int>> = new { new { ["a"] = 1 } }"####,
        Some(r####"{"a":[{"a":1}]}"####),
    ),
    (
        r####"a: Listing<Mapping<String, Int>> = new { new { ["a"] = "b" } }"####,
        None,
    ),
    (
        r####"a: Listing<Int> = new { 1; 2 }"####,
        Some(r####"{"a":[1,2]}"####),
    ),
    (r####"a: Listing<Int> = new { 1; "a" }"####, None),
    // A generic alias argument is expanded before a function body keeps it.
    (
        r####"typealias L<T> = List<T>; function f(x: L<Int>): Int = 1; a = f(List(1))"####,
        Some(r####"{"a":1}"####),
    ),
    // Untyped `new` elements inside `for` and `when` bodies take the element type.
    (
        r####"a: Listing<Listing<Int>> = new { for (i in List(1)) { new { i } } }"####,
        Some(r####"{"a":[[1]]}"####),
    ),
    (
        r####"a: Listing<Listing<Int>> = new { when (true) { new { 1 } } else { new { 2 } } }"####,
        Some(r####"{"a":[[1]]}"####),
    ),
    (
        r####"a: Listing<Listing<Int>> = new { when (false) { new { 1 } } else { new { 2 } } }"####,
        Some(r####"{"a":[[2]]}"####),
    ),
    // An explicit `new Listing` makes its untyped `new` a Dynamic, as in pkl.
    (
        r####"a: Listing<Listing<Int>> = new Listing { new { 1 } }"####,
        None,
    ),
    // A user class shadows the built-in type of the same name.
    (r####"class Int {}; a: Int = 1"####, None),
    (
        r####"class String {}; a = "x" is String"####,
        Some(r####"{"a":false}"####),
    ),
    // A quoted `_` is not a binding, in pkl as here.
    (r####"a = let (`_` = 1) `_`"####, None),
    // `IntSeq` is its own class, not a `List`.
    (
        r####"a = List(1) is IntSeq"####,
        Some(r####"{"a":false}"####),
    ),
    (
        r####"a = IntSeq(1, 3) is IntSeq"####,
        Some(r####"{"a":true}"####),
    ),
    (
        r####"a = IntSeq(1, 3) is List"####,
        Some(r####"{"a":false}"####),
    ),
    (r####"a = List(1) as IntSeq"####, None),
    (
        r####"function f(x: IntSeq): Int = 1; a = f(List(1))"####,
        None,
    ),
    (
        "typealias L<T> = Listing<\"T\">(length == 1)\na: L<Int> = new Listing { \"T\" }",
        Some(r####"{"a":["T"]}"####),
    ),
    (
        "typealias L<T> = Listing<\"T\">(length == 1)\na: L<Int> = new Listing { \"unknown\" }",
        None,
    ),
    (
        "typealias L<T> = Listing<\"a\\\"T\">\na: L<Int> = new Listing { \"a\\\"T\" }",
        Some(r####"{"a":["a\"T"]}"####),
    ),
    (
        r####"a: Mapping<String, Listing<Listing<Int>>> = new { ["k"] = new { new { 1 } } }"####,
        Some(r####"{"a":{"k":[[1]]}}"####),
    ),
    (
        r####"a: Mapping<String, Listing<Listing<Int>>> = new { ["k"] { new { 1 } } }"####,
        Some(r####"{"a":{"k":[[1]]}}"####),
    ),
    (
        r####"a: Mapping<String, Listing<Listing<Int>>> = new { ["k"] = new { new { "x" } } }"####,
        None,
    ),
    (
        r####"a: Mapping<String, Mapping<String, Listing<Int>>> = new { ["k"] = new { ["j"] = new { 1 } } }"####,
        Some(r####"{"a":{"k":{"j":[1]}}}"####),
    ),
    (
        r####"a: Mapping<String, Mapping<String, Listing<Int>>> = new { ["k"] { ["j"] { 1 } } }"####,
        Some(r####"{"a":{"k":{"j":[1]}}}"####),
    ),
    (
        r####"a: Mapping<String, Mapping<String, Listing<Int>>> = new { ["k"] { ["j"] = new { 1 } } }"####,
        Some(r####"{"a":{"k":{"j":[1]}}}"####),
    ),
    (
        r####"a: Mapping<String, Mapping<String, Listing<Int>>> = new { ["k"] = new { ["j"] { 1 } } }"####,
        Some(r####"{"a":{"k":{"j":[1]}}}"####),
    ),
    (
        "class C { x: Int }\na: Mapping<String, Listing<C>> = new { [\"k\"] = new { new { x = 1 } } }",
        Some(r####"{"a":{"k":[{"x":1}]}}"####),
    ),
    (
        "class C { x: Int }\na: Mapping<String, Mapping<String, Listing<C>>> = new { [\"k\"] { [\"j\"] { new { x = 1 } } } }",
        Some(r####"{"a":{"k":{"j":[{"x":1}]}}}"####),
    ),
    (
        "class C { x: Int }\na: Mapping<String, Mapping<String, Listing<C>>> = new { [\"k\"] = new { [\"j\"] = new { new { x = 1 } } } }",
        Some(r####"{"a":{"k":{"j":[{"x":1}]}}}"####),
    ),
    (
        r####"a: Mapping<String, Mapping<String, Listing<Listing<Int>>>> = new { ["k"] { ["j"] { new { 1 } } } }"####,
        Some(r####"{"a":{"k":{"j":[[1]]}}}"####),
    ),
    (
        r####"a: Mapping<String, Listing<Listing<Int>>> = new { for (i in List(1, 2)) { ["k\(i)"] = new { new { i } } } when (true) { ["w"] { new { 3 } } } }"####,
        Some(r####"{"a":{"k1":[[1]],"k2":[[2]],"w":[[3]]}}"####),
    ),
    (
        r####"a: Mapping<String, Listing<Listing<Int>>> = new { for (i in List(1, 2)) { ["k\(i)"] { new { i } } } }"####,
        Some(r####"{"a":{"k1":[[1]],"k2":[[2]]}}"####),
    ),
    (
        r####"a: Mapping<String, Listing<Listing<Int>>> = new { when (true) { ["w"] = new { new { 3 } } } else { ["w"] = new { new { 4 } } } }"####,
        Some(r####"{"a":{"w":[[3]]}}"####),
    ),
    (
        "class C { xs: Listing<Listing<Int>> }\na = new C { xs { new { 1 } } }",
        Some(r####"{"a":{"xs":[[1]]}}"####),
    ),
    (
        "class C { xs: Listing<Listing<Int>> }\na = new C { xs = new { new { 1 } } }",
        Some(r####"{"a":{"xs":[[1]]}}"####),
    ),
    (
        "class C { xs: Listing<Listing<Int>> }\na = new C { xs { new { \"x\" } } }",
        None,
    ),
    (
        "class C { xs: Listing<Listing<Int>> }\na = new C { xs = new { new { \"x\" } } }",
        None,
    ),
    (
        "class D { x: Int }\nclass C { xs: Listing<D> }\na = new C { xs { new { x = 1 } } }",
        Some(r####"{"a":{"xs":[{"x":1}]}}"####),
    ),
    (
        "class C { xs: Listing<Listing<Int>> }\na = new C { xs { for (i in List(1, 2)) { new { i } } when (true) { new { 3 } } } }",
        Some(r####"{"a":{"xs":[[1],[2],[3]]}}"####),
    ),
    (
        "class C { xs: Listing<Listing<Int>> }\na = new C { xs { when (false) { new { 1 } } else { new { 2 } } } }",
        Some(r####"{"a":{"xs":[[2]]}}"####),
    ),
    (
        "class C { xs: Listing<Listing<Int>> = new { new { 1 } } }\nb = new C {}\na = (b) { xs { new { 2 } } }",
        Some(r####"{"b":{"xs":[[1]]},"a":{"xs":[[1],[2]]}}"####),
    ),
    (
        "class C { xs: Listing<Listing<Int>> = new { new { 1 } } }\nb = new C {}\na = (b.xs) { new { 2 } }",
        Some(r####"{"b":{"xs":[[1]]},"a":[[1],[2]]}"####),
    ),
    (
        "open class C { xs: Listing<Listing<Int>> }\nclass S extends C { xs = new { new { 1 } } }\na = new S { xs { new { 2 } } }",
        Some(r####"{"a":{"xs":[[1],[2]]}}"####),
    ),
    (
        "open class C { xs: Listing<Listing<Int>> = new { new { 1 } } }\nclass S extends C {}\na = new S { xs { new { 6 } } }\nb = new S { xs = new { new { 7 } } }",
        Some(r####"{"a":{"xs":[[1],[6]]},"b":{"xs":[[1],[7]]}}"####),
    ),
    (
        "class C { m: Mapping<String, Listing<Listing<Int>>> }\na = new C { m { [\"k\"] { new { 1 } } [\"j\"] = new { new { 2 } } } }",
        Some(r####"{"a":{"m":{"k":[[1]],"j":[[2]]}}}"####),
    ),
];

const KEY_CASES: &[(&str, Option<&str>)] = &[
    (
        r####"hidden a: Mapping<Int, String> = new { [1] = "a" }; b = a.length"####,
        Some(r####"{"b":1}"####),
    ),
    (
        r####"hidden a: Mapping<Int, String> = new { ["a"] = "a" }; b = a.length"####,
        None,
    ),
    (
        r####"hidden a: Mapping<Boolean, String> = new { [true] = "a" }; b = a.length"####,
        Some(r####"{"b":1}"####),
    ),
    (
        r####"hidden a: Mapping<Boolean, String> = new { [1] = "a" }; b = a.length"####,
        None,
    ),
    (
        r####"hidden a: Mapping<Number, String> = new { [1] = "a"; [2.5] = "b" }; b = a.length"####,
        Some(r####"{"b":2}"####),
    ),
    (
        r####"hidden a: Mapping<Number, String> = new { ["x"] = "a" }; b = a.length"####,
        None,
    ),
    (
        r####"hidden a: Mapping<Duration, String> = new { [1.s] = "a" }; b = a.length"####,
        Some(r####"{"b":1}"####),
    ),
    (
        r####"hidden a: Mapping<Duration, String> = new { [1] = "a" }; b = a.length"####,
        None,
    ),
    (
        r####"hidden a: Mapping<Any, Any> = new { [1] = 1; ["b"] = "c" }; b = a.length"####,
        Some(r####"{"b":2}"####),
    ),
    (
        r####"hidden a: Mapping<String|Int, Int> = new Mapping { ["a"] = 1; [1] = 2 }; b = a.length"####,
        Some(r####"{"b":2}"####),
    ),
    (
        r####"hidden a: Mapping<String|Int, Int> = new Mapping { ["a"] = 1; [true] = 2 }; b = a.length"####,
        None,
    ),
    (
        r####"hidden a: Mapping<Int, Int(this > 0)> = new Mapping { [1] = 1 }; b = a.length"####,
        Some(r####"{"b":1}"####),
    ),
    (
        r####"hidden a: Mapping<Int(this > 0), Int> = new Mapping { [0] = 1 }; b = a.length"####,
        None,
    ),
    (
        r####"hidden a: Mapping<Int(this > 0), Int> = new Mapping { [1] = 1 }; b = a.length"####,
        Some(r####"{"b":1}"####),
    ),
    (
        r####"function f(x: Mapping<Int, String>): Int = x.length; a = f(new Mapping { [1] = "a" })"####,
        Some(r####"{"a":1}"####),
    ),
    (
        r####"function f(x: Mapping<Int, String>): Int = x.length; a = f(new Mapping { ["k"] = "a" })"####,
        None,
    ),
    (
        r####"hidden a: Map<Int, String> = Map(1, "a"); b = a.length"####,
        Some(r####"{"b":1}"####),
    ),
    (
        r####"hidden a: Map<Int, String> = Map("a", "a"); b = a.length"####,
        None,
    ),
];

/// `json` without the members that are null, which pkl does not render.
fn without_nulls(json: serde_json::Value) -> serde_json::Value {
    match json {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, without_nulls(v)))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(without_nulls).collect())
        }
        other => other,
    }
}

#[test]
fn element_and_signature_types_match_pkl() {
    let mut failures = Vec::new();
    for (src, expected) in CASES.iter().chain(KEY_CASES) {
        let mut ev = Evaluator::new();
        let result = ev.eval_source(src, std::path::Path::new("test.pkl"));
        match (result, expected) {
            (Err(_), None) => {}
            (Ok(value), Some(json)) => {
                let json: serde_json::Value = serde_json::from_str(json).unwrap();
                let got = without_nulls(value.to_json());
                if got != json {
                    failures.push(format!("{src}: got {got}, expected {json}"));
                }
            }
            (Ok(value), None) => {
                failures.push(format!("{src}: expected an error, got {}", value.to_json()))
            }
            (Err(error), Some(_)) => failures.push(format!("{src}: unexpected error {error}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_dynamic_is_not_accepted_for_a_listing_parameter() {
    let error = eval_fails("function f(x: Listing<Int>): Int = 1\ny = f(new Dynamic { 1 })\n");
    assert!(
        error.contains("Expected value of type `Listing<Int>`, but got type `Dynamic`"),
        "{error}"
    );
}

#[test]
fn a_wrong_element_is_named_in_the_error() {
    let error = eval_fails("a: Listing<Int> = new Listing { 1; \"x\" }\n");
    assert!(
        error.contains("Expected value of type `Listing<Int>`"),
        "{error}"
    );
    let error = eval_fails("function f(x: Int): Int = x\ny = f(\"x\")\n");
    assert!(
        error.contains("Expected value of type `Int`, but got type `String`"),
        "{error}"
    );
}

// pkl checks a `Listing`'s elements when they are read, pklr when the value is
// checked: an ill-typed element is an error even if nothing reads it.
#[test]
fn elements_are_checked_when_the_value_is() {
    let error =
        eval_fails("function f(x: Listing<Int>): Int = x.length\ny = f(new Listing { \"a\" })\n");
    assert!(error.contains("Listing<Int>"), "{error}");
}

// An untyped `new { ... }` element of a class-typed `Listing` is an instance
// of that class in pkl. pklr does not always build it as one, so it accepts
// it, but still rejects a `new Dynamic`.
#[test]
fn untyped_bodies_in_class_typed_collections_are_accepted() {
    let json = eval(
        "class D { path: String? }\n\
         class S { diagnostics: Listing<D> = new Listing<D> {} }\n\
         a = new S { diagnostics { new { path = \"a\" } } }\n",
    );
    assert_eq!(json["a"]["diagnostics"][0]["path"], "a");
    eval_fails(
        "class D { path: String? }\n\
         a: Listing<D> = new Listing { new Dynamic { path = \"a\" } }\n",
    );
}

#[test]
fn type_parameters_of_an_alias_do_not_resolve_as_aliases() {
    let json = eval(
        "typealias Pair<Foo> = List<Foo>\n\
         typealias Foo = Pair<Int>\n\
         x: Foo = List(1)\n",
    );
    assert_eq!(json["x"], serde_json::json!([1]));
}

#[test]
fn function_typed_parameters_accept_std_values() {
    for src in [
        "function f(x: IntSeq): Int = 1\na = f(IntSeq(1, 3))\n",
        "function f(x: Mixin<Dynamic>): Int = 1\na = f(new Mixin { a = 1 })\n",
        "function f(x: Function1<Int, Int>): Int = 1\na = f((y) -> y)\n",
        "function f(x: Duration): Int = 1\na = f(5.s)\n",
        "function f(x: DataSize): Int = 1\na = f(5.mb)\n",
        "function f(x: Regex): Int = 1\na = f(Regex(\"a\"))\n",
        "function f(x: Number): Int = 1\na = f(1.5)\n",
    ] {
        assert_eq!(eval(src)["a"], 1, "{src}");
    }
    eval_fails("function f(x: Function1<Int, Int>): Int = 1\na = f(1)\n");
}

// pkl infers the element type of an index assignment in a listing amendment
// (`[0] = new { 2 }`) like that of a plain element (checked with pkl 0.32.2).
#[test]
fn index_assignments_infer_nested_listing_elements() {
    let json = eval(
        "class C { l: Listing<Listing<Int>> = new { new { 1 } } }\n\
         c: C = new { l { [0] = new { 2 } } }\n",
    );
    assert_eq!(json["c"]["l"], serde_json::json!([[2]]));
    eval_fails(
        "class C { l: Listing<Listing<Int>> = new { new { 1 } } }\n\
         c: C = new { l { [0] = new { \"x\" } } }\n",
    );
}

// An untyped mapping value of a nested listing type amends the mapping's
// `default` instead of replacing it. pkl takes `default` as a function and
// applies it the same way (`default = (k) -> new Listing { ... }` gives
// `[[9], [1]]`); pklr also accepts a listing as the `default`, which pkl
// rejects.
#[test]
fn nested_listing_values_amend_the_mapping_default() {
    let json = eval(
        "a: Mapping<String, Listing<Listing<Int>>> = new {\n\
           default = new Listing { new Listing { 9 } }\n\
           [\"k\"] = new { new Listing { 1 } }\n\
           [\"e\"] = new {}\n\
           [\"j\"] { new Listing { 2 } }\n\
           for (i in List(3)) { [\"f\"] = new { new Listing { i } } }\n\
           when (true) { [\"w\"] = new { new Listing { 4 } } }\n\
         }\n",
    );
    assert_eq!(
        json["a"],
        serde_json::json!({
            "k": [[9], [1]], "e": [[9]], "j": [[9], [2]], "f": [[9], [3]], "w": [[9], [4]]
        })
    );
    eval_fails(
        "a: Mapping<String, Listing<Listing<Int>>> = new {\n\
           default = new Listing { new Listing { 9 } }\n\
           [\"k\"] = new { new Listing { \"x\" } }\n\
         }\n",
    );
}

// An index amendment (`[0] { new { 2 } }`) of a listing infers the nested
// element type like an index assignment does (checked with pkl 0.32.2). pkl
// rejects the form when the amended element has no nested collection type
// (`Listing<Listing<Int>>` with `[0] { new { 2 } }`), as pklr does.
#[test]
fn index_amendments_infer_nested_listing_elements() {
    let json = eval(
        "class C { l: Listing<Listing<Listing<Int>>> = new { new { new { 1 } } } }\n\
         c: C = new { l { [0] { new { 2 } } } }\n",
    );
    assert_eq!(json["c"]["l"], serde_json::json!([[[1], [2]]]));
    let json = eval(
        "class D { x: Int }\n\
         class C { l: Listing<Listing<D>> = new { new { new { x = 1 } } } }\n\
         c: C = new { l { [0] { new { x = 2 } } } }\n",
    );
    assert_eq!(json["c"]["l"], serde_json::json!([[{"x": 1}, {"x": 2}]]));
    eval_fails(
        "class C { l: Listing<Listing<Int>> = new { new { 1 } } }\n\
         c: C = new { l { [0] { new { 2 } } } }\n",
    );
}

// An index amendment of a `Mapping` element amends its entries, it does not
// replace them (checked with pkl 0.32.2).
#[test]
fn index_amendments_amend_mapping_element_entries() {
    let json = eval(
        "class C { l: Listing<Mapping<String, Listing<Int>>> = new { new { [\"k\"] { 1 } } } }\n\
         c: C = new { l { [0] { [\"k\"] { 2 } [\"n\"] = new { 3 } } } }\n",
    );
    assert_eq!(json["c"]["l"], serde_json::json!([{"k": [1, 2], "n": [3]}]));
    let json = eval(
        "class C { l: Listing<Mapping<String, Listing<Listing<Int>>>> = \
           new { new { [\"k\"] { new { 1 } } } } }\n\
         c: C = new { l { [0] { [\"k\"] { new { 2 } } [\"n\"] { new { 3 } } } } }\n",
    );
    assert_eq!(
        json["c"]["l"],
        serde_json::json!([{"k": [[1], [2]], "n": [[3]]}])
    );
}

// An untyped `new` that an `if` or `let` picks as a member of a nested
// listing takes the member type (checked with pkl 0.32.2). pkl rejects a
// parenthesised untyped `new`; pklr still accepts it (see #393).
#[test]
fn selected_untyped_new_members_infer_nested_listing_elements() {
    for src in [
        "a: Listing<Listing<Int>> = new { if (true) new { 1 } else new { 2 } }\n",
        "a: Listing<Listing<Int>> = new { let (x = 1) new { x } }\n",
        "a: Listing<Listing<Int>> = new { when (true) { new { 1 } } }\n",
    ] {
        assert_eq!(eval(src)["a"], serde_json::json!([[1]]), "{src}");
    }
    let json = eval(
        "b: Mapping<String, Listing<Int>> = new { [\"k\"] = if (true) new { 1 } else new { 2 } }\n",
    );
    assert_eq!(json["b"], serde_json::json!({"k": [1]}));
    eval_fails("a: Listing<Listing<Int>> = new { if (true) new { \"x\" } else new { 2 } }\n");
}

// An `IntSeq` is neither a `List` nor a `Collection`, and errors name it
// (checked with pkl 0.32.2).
#[test]
fn an_int_seq_is_not_a_collection() {
    assert_eq!(eval("a = IntSeq(1, 3) is Collection\n")["a"], false);
    assert_eq!(eval("a = IntSeq(1, 3) is List\n")["a"], false);
    let error = eval_fails("function f(x: Collection<Int>): Int = 1\ny = f(IntSeq(1, 3))\n");
    assert!(error.contains("but got type `IntSeq`"), "{error}");
    let error = eval_fails("function f(x: List<Int>): Int = 1\ny = f(IntSeq(1, 3))\n");
    assert!(error.contains("but got type `IntSeq`"), "{error}");
    assert_eq!(
        eval("a = IntSeq(1, 3).getClass().simpleName\n")["a"],
        "IntSeq"
    );
}

// A mapping keyed by a class accepts `Mapping<Class, Int>`: the key is the
// class `String` (checked with pkl 0.32.2, which only fails to render it as
// JSON).
#[test]
fn class_keys_are_checked_as_classes() {
    eval("a: Mapping<Class, Int> = new { [String] = 1 }\nb = a.length\n");
    eval_fails("a: Mapping<String, Int> = new { [String] = 1 }\nb = a.length\n");
}

// The aliases in the type arguments of a constrained base are expanded
// (checked with pkl 0.32.2).
#[test]
fn aliases_in_constrained_generic_bases_are_expanded() {
    let src = "typealias I = Int\n\
               function f(x: List<I>(length == 1)): Int = 1\n";
    assert_eq!(eval(&format!("{src}a = f(List(1))\n"))["a"], 1);
    eval_fails(&format!("{src}a = f(List(\"wrong\"))\n"));
}

// A union's default alternative keeps its type arguments: `*Listing<Int>`
// is checked as a `Listing<Int>` (checked with pkl 0.32.2).
#[test]
fn default_union_alternatives_keep_their_type_arguments() {
    let src = "function f(x: *Listing<Int> | String): Int = 1\n";
    assert_eq!(eval(&format!("{src}a = f(\"s\")\n"))["a"], 1);
    eval_fails(&format!("{src}a = f(true)\n"));
    eval_fails(
        "class C { m: *Mapping<String, Int> | String = new Mapping { [\"a\"] = \"x\" } }\nc = new C {}\n",
    );
    eval_fails("class C { l: *Listing<Int> | String = new Listing { \"x\" } }\nc = new C {}\n");
}

// An untyped `new` that an `if` or `let` picks as a mapping value amends the
// mapping's `default` like a plain `new` does. pkl needs a function `default`
// for this; pklr also accepts a listing.
#[test]
fn selected_untyped_new_mapping_values_amend_the_default() {
    let json = eval(
        "a: Mapping<String, Listing<Listing<Int>>> = new {\n\
           default = new Listing { new Listing { 9 } }\n\
           [\"k\"] = if (true) new { new Listing { 1 } } else new {}\n\
           [\"j\"] = let (x = 2) new { new Listing { x } }\n\
           [\"m\"] = new { new Listing { 3 } }\n\
         }\n",
    );
    assert_eq!(
        json["a"],
        serde_json::json!({"k": [[9], [1]], "j": [[9], [2]], "m": [[9], [3]]})
    );
}
