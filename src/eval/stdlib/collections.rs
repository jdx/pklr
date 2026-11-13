//! The methods of `List` and `Set` that `eval_method_call` does not answer
//! itself, and the few `Listing` ones it lacks. They are reached only after
//! the common methods missed, so none of this is on a hot path.

use super::*;
use crate::eval::render::display_value;
use crate::value::{DataSize, DataSizeUnit};

impl Evaluator {
    /// Call the method `name` of the list, set or listing `items`, or `None`
    /// when its type declares no such method.
    #[inline(never)]
    pub(in crate::eval) fn collection_method(
        &mut self,
        items: &ListValue,
        name: &str,
        args: &[Value],
        depth: usize,
    ) -> Option<Result<Value>> {
        let kind = items.kind();
        match kind {
            ListKind::List | ListKind::Set => {}
            ListKind::Listing => {
                if !matches!(
                    name,
                    "getOrNull" | "distinctBy" | "isDistinctBy" | "foldIndexed"
                ) {
                    return None;
                }
            }
            ListKind::Dynamic => {
                // `Dynamic.length()` counts the elements.
                return (name == "length" && args.is_empty())
                    .then(|| Ok(Value::Int(items.len() as i64)));
            }
        }
        self.collection_method_inner(items, name, args, depth)
            .transpose()
    }

    /// Call a `Map` method the common arms of `eval_method_call` do not
    /// answer, or `None` when `Map` declares no such method.
    #[inline(never)]
    pub(in crate::eval) fn map_method(
        &mut self,
        map: &ObjectMap,
        name: &str,
        args: &[Value],
        depth: usize,
    ) -> Option<Result<Value>> {
        if !matches!(
            name,
            "getOrNull"
                | "containsValue"
                | "put"
                | "remove"
                | "mapKeys"
                | "map"
                | "flatMap"
                | "fold"
        ) {
            return None;
        }
        Some(self.map_method_inner(map, name, args, depth))
    }

    fn map_insert(&mut self, map: &mut ObjectMap, key: &Value, value: Value) -> Result<()> {
        self.raise_deep_failed(key)?;
        let stored = value_to_key(key)?;
        let storage_key = equivalent_mapping_key(map, &stored).unwrap_or(stored);
        self.remember_mapping_key(storage_key.clone(), key);
        map.insert(storage_key, value.into_entry_value());
        Ok(())
    }

    fn map_method_inner(
        &mut self,
        map: &ObjectMap,
        name: &str,
        args: &[Value],
        depth: usize,
    ) -> Result<Value> {
        let entry_key = |this: &Self, stored: &str| this.entry_key_value(stored);
        Ok(match name {
            "getOrNull" | "remove" | "put" => {
                let expected = if name == "put" { 2 } else { 1 };
                if args.len() != expected {
                    return Err(arity_error(expected, args));
                }
                let stored = value_to_key(&args[0])?;
                let found = equivalent_mapping_key(map, &stored);
                match name {
                    "getOrNull" => found
                        .and_then(|key| map.get(&key).cloned())
                        .unwrap_or(Value::Null),
                    "remove" => {
                        let mut out = ObjectMap::default();
                        for (key, value) in map {
                            if found.as_ref() != Some(key) {
                                out.insert(key.clone(), value.clone());
                            }
                        }
                        Value::Object(Arc::new(out), None)
                    }
                    _ => {
                        let mut out = map.clone();
                        self.map_insert(&mut out, &args[0], args[1].clone())?;
                        Value::Object(Arc::new(out), None)
                    }
                }
            }
            "containsValue" => {
                let [wanted] = args else {
                    return Err(arity_error(1, args));
                };
                Value::Bool(map.values().any(|value| values_eq(value, wanted)))
            }
            "fold" => {
                let [initial, operator] = args else {
                    return Err(arity_error(2, args));
                };
                let mut acc = initial.clone();
                for (key, value) in map {
                    let key = entry_key(self, key);
                    acc = self.invoke_lambda(operator, &[acc, key, value.clone()], depth)?;
                }
                acc
            }
            "mapKeys" | "map" | "flatMap" => {
                let transform = lambda_arg(args, 1)?;
                let mut out = ObjectMap::default();
                for (key, value) in map {
                    let key = entry_key(self, key);
                    let result =
                        self.invoke_lambda(transform, &[key.clone(), value.clone()], depth)?;
                    match name {
                        "mapKeys" => self.map_insert(&mut out, &result, value.clone())?,
                        "map" => {
                            if !is_pair(&result) {
                                return Err(type_mismatch("Pair", &result));
                            }
                            self.map_insert(&mut out, &pair_first(&result), pair_second(&result))?;
                        }
                        _ => {
                            let Value::Object(entries, None) = &result else {
                                return Err(type_mismatch("Map", &result));
                            };
                            for (stored, value) in entries.iter() {
                                let key = entry_key(self, stored);
                                self.map_insert(&mut out, &key, value.clone())?;
                            }
                        }
                    }
                }
                Value::Object(Arc::new(out), None)
            }
            _ => unreachable!("map_method filters the names"),
        })
    }

    fn collection_method_inner(
        &mut self,
        items: &ListValue,
        name: &str,
        args: &[Value],
        depth: usize,
    ) -> Result<Option<Value>> {
        let kind = items.kind();
        let this = || Value::List(items.clone());
        let same = |elements: Vec<Value>| collection_result(kind, elements);
        let list = |elements: Vec<Value>| Value::List(ListValue::new(ListKind::List, elements));
        let slice: &[Value] = items;
        let value = match name {
            "getOrNull" => {
                let [index] = args else {
                    return Err(arity_error(1, args));
                };
                let index = int_arg(index)?;
                usize::try_from(index)
                    .ok()
                    .and_then(|i| slice.get(i))
                    .cloned()
                    .unwrap_or(Value::Null)
            }
            "sublist" | "sublistOrNull" => {
                let [start, end] = args else {
                    return Err(arity_error(2, args));
                };
                let (start, end) = (int_arg(start)?, int_arg(end)?);
                let len = slice.len() as i64;
                if start < 0 || start > len {
                    return or_null(name, Err(index_error(start, 0, len, &this())));
                }
                if end < start || end > len {
                    return or_null(name, Err(index_error(end, start, len, &this())));
                }
                list(slice[start as usize..end as usize].to_vec())
            }
            "startsWith" | "endsWith" => {
                let [other] = args else {
                    return Err(arity_error(1, args));
                };
                let Value::List(other) = other else {
                    return Err(type_mismatch("Collection", other));
                };
                let other: &[Value] = other;
                let aligned = if name == "startsWith" {
                    slice.get(..other.len())
                } else {
                    slice.len().checked_sub(other.len()).map(|at| &slice[at..])
                };
                Value::Bool(
                    aligned
                        .is_some_and(|part| part.iter().zip(other).all(|(a, b)| values_eq(a, b))),
                )
            }
            "split" | "splitOrNull" => {
                let [index] = args else {
                    return Err(arity_error(1, args));
                };
                let index = int_arg(index)?;
                let len = slice.len() as i64;
                if index < 0 || index > len {
                    return or_null(name, Err(index_error(index, 0, len, &this())));
                }
                let (a, b) = slice.split_at(index as usize);
                pair(same(a.to_vec()), same(b.to_vec()))
            }
            "partition" => {
                let predicate = lambda_arg(args, 1)?;
                let (mut yes, mut no) = (Vec::new(), Vec::new());
                for item in slice {
                    if self.call_predicate(predicate, std::slice::from_ref(item), depth)? {
                        yes.push(item.clone());
                    } else {
                        no.push(item.clone());
                    }
                }
                pair(same(yes), same(no))
            }
            "mapNonNull" => {
                let transform = lambda_arg(args, 1)?;
                let mut out = Vec::new();
                for item in slice {
                    let value = self.invoke_lambda(transform, std::slice::from_ref(item), depth)?;
                    if !matches!(value, Value::Null) {
                        out.push(value);
                    }
                }
                same(out)
            }
            "filterIndexed" | "mapIndexed" | "mapNonNullIndexed" | "flatMapIndexed" => {
                let f = lambda_arg(args, 2)?;
                let mut out = Vec::new();
                for (i, item) in slice.iter().enumerate() {
                    let index = Value::Int(i as i64);
                    let value = self.invoke_lambda(f, &[index, item.clone()], depth)?;
                    match name {
                        "filterIndexed" => {
                            let Value::Bool(keep) = value else {
                                return Err(type_mismatch("Boolean", &value));
                            };
                            if keep {
                                out.push(item.clone());
                            }
                        }
                        "mapIndexed" => out.push(value),
                        "mapNonNullIndexed" => {
                            if !matches!(value, Value::Null) {
                                out.push(value);
                            }
                        }
                        _ => out.extend(collection_items(&value)?),
                    }
                }
                same(out)
            }
            "isDistinctBy" | "distinctBy" => {
                let selector = lambda_arg(args, 1)?;
                let mut seen: Vec<Value> = Vec::new();
                let mut out = Vec::new();
                for item in slice {
                    let key = self.invoke_lambda(selector, std::slice::from_ref(item), depth)?;
                    if set_contains(&seen, &key) {
                        if name == "isDistinctBy" {
                            return Ok(Some(Value::Bool(false)));
                        }
                        continue;
                    }
                    seen.push(key);
                    out.push(item.clone());
                }
                if name == "isDistinctBy" {
                    Value::Bool(true)
                } else {
                    Value::List(ListValue::new(kind, out))
                }
            }
            "flatten" => {
                let mut out = Vec::new();
                for item in slice {
                    match item {
                        Value::List(inner) => out.extend(inner.iter().cloned()),
                        other => {
                            return Err(error_with_values(
                                "Cannot flatten a collection containing a non-collection element.",
                                &[("Element", other)],
                            ));
                        }
                    }
                }
                same(out)
            }
            "replace" | "replaceOrNull" => {
                let [index, replacement] = args else {
                    return Err(arity_error(2, args));
                };
                let index = int_arg(index)?;
                let len = slice.len() as i64;
                if index < 0 || index >= len {
                    return or_null(name, Err(index_error(index, 0, (len - 1).max(0), &this())));
                }
                let mut out = slice.to_vec();
                out[index as usize] = replacement.clone();
                list(out)
            }
            "replaceRange" | "replaceRangeOrNull" => {
                let [start, end, replacement] = args else {
                    return Err(arity_error(3, args));
                };
                let (start, end) = (int_arg(start)?, int_arg(end)?);
                let len = slice.len() as i64;
                if start < 0 || start > len {
                    return or_null(name, Err(index_error(start, 0, len, &this())));
                }
                if end < start || end > len {
                    return or_null(name, Err(index_error(end, start, len, &this())));
                }
                let mut out = slice[..start as usize].to_vec();
                out.extend(collection_items(replacement)?);
                out.extend_from_slice(&slice[end as usize..]);
                list(out)
            }
            "find"
            | "findOrNull"
            | "findLast"
            | "findLastOrNull"
            | "findIndex"
            | "findIndexOrNull"
            | "findLastIndex"
            | "findLastIndexOrNull" => {
                let predicate = lambda_arg(args, 1)?;
                let last = name.starts_with("findLast");
                let mut found = None;
                let order: Box<dyn Iterator<Item = usize>> = if last {
                    Box::new((0..slice.len()).rev())
                } else {
                    Box::new(0..slice.len())
                };
                for i in order {
                    if self.call_predicate(predicate, std::slice::from_ref(&slice[i]), depth)? {
                        found = Some(i);
                        break;
                    }
                }
                match found {
                    Some(i) if name.contains("Index") => Value::Int(i as i64),
                    Some(i) => slice[i].clone(),
                    None if name.ends_with("OrNull") => Value::Null,
                    None => {
                        return Err(Error::Eval(format!(
                            "Cannot find matching element in collection.\nCollection: {}",
                            display_value(&this())
                        )));
                    }
                }
            }
            "indexOf" | "indexOfOrNull" | "lastIndexOf" | "lastIndexOfOrNull" => {
                let [element] = args else {
                    return Err(arity_error(1, args));
                };
                let matches = |v: &&Value| values_eq(v, element);
                let found = if name.starts_with("last") {
                    slice.iter().rposition(|v| matches(&v))
                } else {
                    slice.iter().position(|v| matches(&v))
                };
                match found {
                    Some(i) => Value::Int(i as i64),
                    None if name.ends_with("OrNull") => Value::Null,
                    None => {
                        return Err(Error::Eval(format!(
                            "Cannot find element in collection.\nElement   : {}\nCollection: {}",
                            display_value(element),
                            display_value(&this())
                        )));
                    }
                }
            }
            "count" => {
                let predicate = lambda_arg(args, 1)?;
                let mut count = 0;
                for item in slice {
                    if self.call_predicate(predicate, std::slice::from_ref(item), depth)? {
                        count += 1;
                    }
                }
                Value::Int(count)
            }
            "take" | "drop" | "takeLast" | "dropLast" | "repeat" => {
                let [n] = args else {
                    return Err(arity_error(1, args));
                };
                let n = int_arg(n)?;
                if n < 0 {
                    return Err(Error::Eval(format!(
                        "Expected a positive number, but got `{n}`."
                    )));
                }
                let n = usize::try_from(n).unwrap_or(usize::MAX);
                let len = slice.len();
                match name {
                    "take" => same(slice[..n.min(len)].to_vec()),
                    "drop" => same(slice[n.min(len)..].to_vec()),
                    "takeLast" => same(slice[len - n.min(len)..].to_vec()),
                    "dropLast" => same(slice[..len - n.min(len)].to_vec()),
                    _ => {
                        let total = n.saturating_mul(len);
                        let mut out = Vec::new();
                        if total > i32::MAX as usize || out.try_reserve_exact(total).is_err() {
                            return Err(Error::Eval("Collection is too large to repeat.".into()));
                        }
                        for _ in 0..n {
                            out.extend_from_slice(slice);
                        }
                        list(out)
                    }
                }
            }
            "takeWhile" | "dropWhile" | "takeLastWhile" | "dropLastWhile" => {
                let predicate = lambda_arg(args, 1)?;
                let from_end = name.contains("Last");
                let mut boundary = if from_end { 0 } else { slice.len() };
                let order: Box<dyn Iterator<Item = usize>> = if from_end {
                    Box::new((0..slice.len()).rev())
                } else {
                    Box::new(0..slice.len())
                };
                for i in order {
                    if !self.call_predicate(predicate, std::slice::from_ref(&slice[i]), depth)? {
                        boundary = if from_end { i + 1 } else { i };
                        break;
                    }
                }
                match name {
                    "takeWhile" => same(slice[..boundary].to_vec()),
                    "dropWhile" => same(slice[boundary..].to_vec()),
                    "takeLastWhile" => same(slice[boundary..].to_vec()),
                    _ => same(slice[..boundary].to_vec()),
                }
            }
            "foldBack" => {
                let [init, f] = args else {
                    return Err(arity_error(2, args));
                };
                let f = lambda_arg(std::slice::from_ref(f), 2)?;
                let mut acc = init.clone();
                for item in slice.iter().rev() {
                    acc = self.invoke_lambda(f, &[item.clone(), acc], depth)?;
                }
                acc
            }
            "foldIndexed" => {
                let [init, f] = args else {
                    return Err(arity_error(2, args));
                };
                let f = lambda_arg(std::slice::from_ref(f), 3)?;
                let mut acc = init.clone();
                for (i, item) in slice.iter().enumerate() {
                    acc =
                        self.invoke_lambda(f, &[Value::Int(i as i64), acc, item.clone()], depth)?;
                }
                acc
            }
            "reduce" | "reduceOrNull" => {
                let f = lambda_arg(args, 2)?;
                let Some((first, rest)) = slice.split_first() else {
                    if name == "reduceOrNull" {
                        return Ok(Some(Value::Null));
                    }
                    return Err(empty_error(&this()));
                };
                let mut acc = first.clone();
                for item in rest {
                    acc = self.invoke_lambda(f, &[acc, item.clone()], depth)?;
                }
                acc
            }
            "groupBy" => {
                let selector = lambda_arg(args, 1)?;
                let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
                for item in slice {
                    let key = self.invoke_lambda(selector, std::slice::from_ref(item), depth)?;
                    match groups.iter_mut().find(|(k, _)| values_eq(k, &key)) {
                        Some((_, group)) => group.push(item.clone()),
                        None => groups.push((key, vec![item.clone()])),
                    }
                }
                let mut map = ObjectMap::default();
                for (key, group) in groups {
                    let storage_key = value_to_key(&key)?;
                    self.remember_mapping_key(storage_key.clone(), &key);
                    map.insert(storage_key, Value::List(ListValue::new(kind, group)));
                }
                Value::Object(Arc::new(map), None)
            }
            "minBy" | "minByOrNull" | "maxBy" | "maxByOrNull" => {
                let selector = lambda_arg(args, 1)?;
                let mut best: Option<(Value, Value)> = None;
                for item in slice {
                    let key = self.invoke_lambda(selector, std::slice::from_ref(item), depth)?;
                    let better = match &best {
                        None => true,
                        Some((best_key, _)) => beats(name.starts_with("min"), &key, best_key)?,
                    };
                    if better {
                        best = Some((key, item.clone()));
                    }
                }
                match best {
                    Some((_, item)) => item,
                    None if name.ends_with("OrNull") => Value::Null,
                    None => return Err(empty_error(&this())),
                }
            }
            "minWith" | "minWithOrNull" | "maxWith" | "maxWithOrNull" => {
                let comparator = lambda_arg(args, 2)?;
                let mut best: Option<&Value> = None;
                for item in slice {
                    let better = match best {
                        None => true,
                        Some(current) => {
                            let (l, r) = if name.starts_with("min") {
                                (item, current)
                            } else {
                                (current, item)
                            };
                            self.comparator_less(comparator, l, r, depth)?
                        }
                    };
                    if better {
                        best = Some(item);
                    }
                }
                match best {
                    Some(item) => item.clone(),
                    None if name.ends_with("OrNull") => Value::Null,
                    None => return Err(empty_error(&this())),
                }
            }
            "sort" => {
                check_arity(args, 0)?;
                list(self.sort_values(slice.to_vec(), &mut |_, a, b| less(a, b))?)
            }
            "sortBy" => {
                let selector = lambda_arg(args, 1)?;
                let mut keyed = Vec::with_capacity(slice.len());
                for item in slice {
                    let key = self.invoke_lambda(selector, std::slice::from_ref(item), depth)?;
                    keyed.push(pair(key, item.clone()));
                }
                let sorted =
                    self.sort_values(keyed, &mut |_, a, b| less(&pair_first(a), &pair_first(b)))?;
                list(sorted.iter().map(pair_second).collect())
            }
            "sortWith" => {
                let comparator = lambda_arg(args, 2)?.clone();
                list(self.sort_values(slice.to_vec(), &mut |this, a, b| {
                    this.comparator_less(&comparator, a, b, depth)
                })?)
            }
            "zip" => {
                let [other] = args else {
                    return Err(arity_error(1, args));
                };
                let Value::List(other) = other else {
                    return Err(type_mismatch("Collection", other));
                };
                same(
                    slice
                        .iter()
                        .zip(other.iter())
                        .map(|(a, b)| pair(a.clone(), b.clone()))
                        .collect(),
                )
            }
            "intersect" | "difference" if kind == ListKind::Set => {
                let [other] = args else {
                    return Err(arity_error(1, args));
                };
                let Value::List(other) = other else {
                    return Err(type_mismatch("Set", other));
                };
                let keep = name == "intersect";
                same(
                    slice
                        .iter()
                        .filter(|item| set_contains(other, item) == keep)
                        .cloned()
                        .collect(),
                )
            }
            "decodeToString" if items.is_bytes() => {
                let [charset] = args else {
                    return Err(arity_error(1, args));
                };
                decode_to_string(items, charset)?
            }
            "toBytes" if kind == ListKind::List => {
                check_arity(args, 0)?;
                let mut bytes = Vec::with_capacity(slice.len());
                for item in slice {
                    match item {
                        Value::Int(0..=255) => bytes.push(item.clone()),
                        Value::Int(n) => {
                            return Err(Error::Eval(format!(
                                "Type constraint `isBetween(0, 255)` violated.\nValue: {n}"
                            )));
                        }
                        other => {
                            return Err(Error::Eval(format!(
                                "Expected value of type `Int`, but got `{}`.",
                                display_value(other)
                            )));
                        }
                    }
                }
                Value::List(ListValue::from(bytes).into_bytes())
            }
            "toDynamic" if kind != ListKind::Listing => {
                check_arity(args, 0)?;
                Value::List(ListValue::new(ListKind::Dynamic, slice.to_vec()))
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    /// Whether `comparator(a, b)` says `a` sorts before `b`. It returns a
    /// `Boolean`, or an `Int` that is negative when `a` is first.
    fn comparator_less(
        &mut self,
        comparator: &Value,
        a: &Value,
        b: &Value,
        depth: usize,
    ) -> Result<bool> {
        match self.invoke_lambda(comparator, &[a.clone(), b.clone()], depth)? {
            Value::Bool(less) => Ok(less),
            Value::Int(n) => Ok(n < 0),
            other => Err(type_mismatch("Boolean", &other)),
        }
    }

    /// A stable merge sort. `less` may fail and need not be a total order,
    /// which `slice::sort_by` would not tolerate.
    fn sort_values(
        &mut self,
        mut items: Vec<Value>,
        less: &mut dyn FnMut(&mut Self, &Value, &Value) -> Result<bool>,
    ) -> Result<Vec<Value>> {
        if items.len() < 2 {
            return Ok(items);
        }
        let right = items.split_off(items.len() / 2);
        let left = self.sort_values(items, less)?;
        let right = self.sort_values(right, less)?;
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let (mut left, mut right) = (left.into_iter().peekable(), right.into_iter().peekable());
        while let (Some(l), Some(r)) = (left.peek(), right.peek()) {
            // The right element goes first only when it is strictly less.
            if less(self, r, l)? {
                merged.extend(right.next());
            } else {
                merged.extend(left.next());
            }
        }
        merged.extend(left);
        merged.extend(right);
        Ok(merged)
    }
}

/// `a < b`, with pkl's errors for operands it cannot order.
fn less(a: &Value, b: &Value) -> Result<bool> {
    Ok(matches!(
        apply_binop(BinOp::Lt, a.clone(), b.clone())?,
        Value::Bool(true)
    ))
}

/// Whether `candidate` replaces `current` as the minimum (`candidate < current`)
/// or the maximum (`candidate > current`).
fn beats(minimum: bool, candidate: &Value, current: &Value) -> Result<bool> {
    let op = if minimum { BinOp::Lt } else { BinOp::Gt };
    Ok(matches!(
        apply_binop(op, candidate.clone(), current.clone())?,
        Value::Bool(true)
    ))
}

fn pair(first: Value, second: Value) -> Value {
    let mut members = ObjectMap::default();
    members.insert("first".into(), first);
    members.insert("second".into(), second);
    typed_object("Pair", members)
}

fn is_pair(value: &Value) -> bool {
    matches!(value, Value::Object(_, Some(source)) if source.type_name.as_deref() == Some("Pair"))
}

fn pair_member(pair: &Value, name: &str) -> Value {
    match pair {
        Value::Object(members, _) => members.get(name).cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn pair_first(pair: &Value) -> Value {
    pair_member(pair, "first")
}

fn pair_second(pair: &Value) -> Value {
    pair_member(pair, "second")
}

fn arity_error(expected: usize, args: &[Value]) -> Error {
    check_arity(args, expected)
        .err()
        .unwrap_or_else(|| Error::Eval(format!("Expected {expected} function arguments.")))
}

fn int_arg(value: &Value) -> Result<i64> {
    match value {
        Value::Int(n) => Ok(*n),
        other => Err(type_mismatch("Int", other)),
    }
}

/// The single function argument of a method, which must take `params`
/// parameters.
fn lambda_arg(args: &[Value], params: usize) -> Result<&Value> {
    let [arg] = args else {
        return Err(arity_error(1, args));
    };
    match arg {
        Value::Lambda(..) => Ok(arg),
        other => Err(Error::Eval(format!(
            "Expected value of type `Function{params}`, but got type `{}`.\nValue: {}",
            other.type_name(),
            display_value(other)
        ))),
    }
}

/// The elements of a collection a function returned.
fn collection_items(value: &Value) -> Result<Vec<Value>> {
    match value {
        Value::List(items) => Ok(items.iter().cloned().collect()),
        other => Err(type_mismatch("Collection", other)),
    }
}

fn or_null(name: &str, error: Result<Value>) -> Result<Option<Value>> {
    if name.ends_with("OrNull") {
        Ok(Some(Value::Null))
    } else {
        error.map(Some)
    }
}

fn empty_error(collection: &Value) -> Error {
    Error::Eval(format!(
        "Expected a non-empty collection.\nCollection: {}",
        display_value(collection)
    ))
}

fn index_error(index: i64, low: i64, high: i64, collection: &Value) -> Error {
    Error::Eval(format!(
        "Element index `{index}` is out of range `{low}`..`{high}`.\nCollection: {}",
        display_value(collection)
    ))
}

/// A property of a list, set or listing that `list_property` does not
/// answer, or `None` when its type declares no such property.
#[inline(never)]
pub(in crate::eval) fn collection_property(items: &ListValue, name: &str) -> Option<Result<Value>> {
    let kind = items.kind();
    if kind == ListKind::Dynamic {
        return None;
    }
    if items.is_bytes()
        && let Some(result) = bytes_property(items, name)
    {
        return Some(result);
    }
    Some(match name {
        "isNotEmpty" => Ok(Value::Bool(!items.is_empty())),
        "distinct" if kind != ListKind::Set => {
            let mut seen: Vec<Value> = Vec::new();
            for item in items.iter() {
                if !set_contains(&seen, item) {
                    seen.push(item.clone());
                }
            }
            Ok(Value::List(ListValue::new(kind, seen)))
        }
        "min" | "max" | "minOrNull" | "maxOrNull" if kind != ListKind::Listing => {
            let mut best: Option<&Value> = None;
            for item in items.iter() {
                let better = match best {
                    None => true,
                    Some(current) => match beats(name.starts_with("min"), item, current) {
                        Ok(better) => better,
                        Err(error) => return Some(Err(error)),
                    },
                };
                if better {
                    best = Some(item);
                }
            }
            match best {
                Some(item) => Ok(item.clone()),
                None if name.ends_with("OrNull") => Ok(Value::Null),
                None => Err(empty_error(&Value::List(items.clone()))),
            }
        }
        _ => return None,
    })
}

/// The bytes of a `Bytes` value.
fn byte_vec(items: &ListValue) -> Vec<u8> {
    items
        .iter()
        .map(|item| match item {
            Value::Int(n) => *n as u8,
            _ => 0,
        })
        .collect()
}

/// A property that `Bytes` has and a `List` does not.
/// `bytes` and the digests of a `Resource`, from its `base64` or `text`.
pub(in crate::eval) fn resource_property(members: &ObjectMap, name: &str) -> Option<Result<Value>> {
    if !matches!(name, "bytes" | "md5" | "sha1" | "sha256" | "sha256Int") {
        return None;
    }
    let bytes = match (
        members.get("bytes"),
        members.get("base64"),
        members.get("text"),
    ) {
        (Some(bytes @ Value::List(items)), ..) if items.is_bytes() => bytes.clone(),
        (_, Some(Value::String(encoded)), _) => {
            super::string::bytes_value(super::string::base64_decode(encoded).ok()?)
        }
        (.., Some(Value::String(text))) => super::string::bytes_value(text.as_bytes().to_vec()),
        _ => return None,
    };
    if name == "bytes" {
        return Some(Ok(bytes));
    }
    let Value::List(items) = &bytes else {
        return None;
    };
    bytes_property(items, name)
}

fn bytes_property(items: &ListValue, name: &str) -> Option<Result<Value>> {
    use super::string::{base64_encode, hex};
    Some(Ok(match name {
        "size" => {
            // The largest decimal unit that is no more than the length.
            let len = items.len() as f64;
            let unit = [
                DataSizeUnit::Petabytes,
                DataSizeUnit::Terabytes,
                DataSizeUnit::Gigabytes,
                DataSizeUnit::Megabytes,
                DataSizeUnit::Kilobytes,
            ]
            .into_iter()
            .find(|unit| len >= unit.bytes() as f64)
            .unwrap_or(DataSizeUnit::Bytes);
            Value::DataSize(DataSize::new(len / unit.bytes() as f64, unit))
        }
        "base64" => Value::String(base64_encode(&byte_vec(items)).into()),
        "hex" => Value::String(hex(&byte_vec(items)).into()),
        "md5" => {
            use md5::Digest;
            Value::String(hex(&md5::Md5::digest(byte_vec(items))).into())
        }
        "sha1" => {
            use sha1::Digest;
            Value::String(hex(&sha1::Sha1::digest(byte_vec(items))).into())
        }
        "sha256" => {
            use sha2::Digest;
            Value::String(hex(&sha2::Sha256::digest(byte_vec(items))).into())
        }
        "sha256Int" => {
            use sha2::Digest;
            let digest = sha2::Sha256::digest(byte_vec(items));
            let mut first = [0u8; 8];
            first.copy_from_slice(&digest[..8]);
            Value::Int(i64::from_le_bytes(first))
        }
        _ => return None,
    }))
}

/// `Bytes.decodeToString(charset)`.
fn decode_to_string(items: &ListValue, charset: &Value) -> Result<Value> {
    let charset = super::string::charset_arg(charset)?;
    let bytes = byte_vec(items);
    let invalid = || Error::Eval(format!("Invalid bytes for charset \"{charset}\"."));
    let text = match charset {
        "UTF-8" => String::from_utf8(bytes).map_err(|_| invalid())?,
        "UTF-16" => {
            let (bom, rest) = match bytes.as_slice() {
                [0xff, 0xfe, rest @ ..] => (true, rest),
                [0xfe, 0xff, rest @ ..] => (false, rest),
                rest => (false, rest),
            };
            if rest.len() % 2 != 0 {
                return Err(invalid());
            }
            let units = rest.chunks(2).map(|c| {
                if bom {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            });
            char::decode_utf16(units)
                .collect::<std::result::Result<String, _>>()
                .map_err(|_| invalid())?
        }
        _ => bytes.iter().map(|b| char::from(*b)).collect(),
    };
    Ok(Value::String(text.into()))
}
