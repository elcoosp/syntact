//! Feature-completeness probe (dev tool): throw a feature-rich realistic
//! sample at every pack's `parse_events` and report error counts so gaps
//! surface as data. Not part of the shipped test suite.
//!
//! Run: `cargo run -p bench --example feature_probe`

/// The uniform `parse_events` shape every reference pack exposes.
type ParseEventsFn = fn(
    &str,
) -> (
    Vec<runtime_core::events::Event>,
    Vec<(u16, std::ops::Range<u32>)>,
    usize,
);

fn probe(name: &str, src: &str, f: ParseEventsFn) {
    let (_, tokens, errors) = f(src);
    let src_len = u32::try_from(src.len()).unwrap_or(u32::MAX);
    let err_tokens: Vec<String> = tokens
        .iter()
        .filter(|(k, _)| *k == runtime_core::ERROR_KIND)
        .map(|(_, r)| {
            let lo = r.start.min(src_len) as usize;
            let hi = r.end.min(src_len) as usize;
            format!("{:?}@{}", &src[lo..hi], r.start)
        })
        .collect();
    println!(
        "{name}: errors={errors} err_regions={err_tokens:?} total_tokens={}",
        tokens.len()
    );
}

#[allow(clippy::too_many_lines)] // one sample per language
fn main() {
    let py = r#"
import os
from collections import OrderedDict as OD, defaultdict

try:
    with open("f.txt") as f, open("g.txt") as g:
        data = f.read()
except (ValueError, KeyError) as exc:
    raise RuntimeError("bad") from exc
finally:
    os.close(0)

total = sum(x*x for x in range(10) if x % 2 == 0)
pairs = {k: v for k, v in items}
uniq = {a for a in xs}
first, *rest = [1, 2, 3]
lam = lambda x, y=2, *args, **kw: x + y
x = a if cond else b
y: int = 0
del x
assert x == 1, "boom"
global counter
match point:
    case (0, 0):
        pass
val = (n := 10) + 5
s = obj.attr_list[1:2][0].name
p, q = q, p

async def fetch(url):
    async with session.get(url) as resp:
        return await resp.text()
"#;
    probe("python", py, packs_python::reference::parse_events);

    let ts = r#"
import def, { a as b, c } from "mod";
import * as ns from "other";

interface P<T extends object = {}> { readonly x?: T; }
type F = (a: string, ...rest: number[]) => void;

enum Color { Red = 1, Green, Blue }

abstract class Base<T> implements P<T> {
    static count = 0;
    #priv = 1;
    constructor(public readonly name: string) { super(); }
    abstract area(): number;
    get size(): number { return this.#priv; }
}

async function* gen(): AsyncGenerator<number> {
    await Promise.resolve();
    yield 1;
}

const obj = { a: 1, ["b"]: 2, ...spread };
const v = obj?.a?.b ?? fallback;
const el = <div className="x">{child}</div>;

try { risky(); } catch (e) { log(e); } finally { done(); }

switch (x) {
    case 1: break;
    default: break;
}

do { step(); } while (c);
outer: for (const k in obj) continue outer;
label2: while (true) break label2;
class D extends Base<string> { area() { return 0; } }
"#;
    probe("typescript", ts, packs_typescript::reference::parse_events);

    let c = r#"
#include <stdio.h>
#define MAX(a, b) ((a) > (b) ? (a) : (b))

typedef struct Node { struct Node *next; int val : 3; } Node;
enum E { A = 1, B = A + 1 };
union U { int i; float f; };
typedef int (*cb)(void *, size_t);

static inline int add(int a, int b) { return a + b; }

int main(int argc, char **argv) {
    switch (argc) {
        case 1: break;
        default: continue_lbl: goto continue_lbl;
    }
    do { work(); } while (0);
    for (int i = 0; i < 10; ++i) { if (i == 5) goto done; }
done:
    int arr[3] = {[1] = 2, .x = 1};
    long big = 1L << 40;
    size_t n = sizeof(struct Node);
    _Bool ok = (void *)0 == NULL;
    char *s = u8"unicode \x41\n";
    return (int)(n % 2);
}
"#;
    probe("c", c, packs_c::reference::parse_events);

    let rust = r#"
use std::collections::HashMap as Map;
use std::fmt::{self, Display};

pub trait Shape: Display + Send + Sync {
    type Output;
    const SIDES: u8 = 4;
    fn area(&self) -> f64;
    fn name(&self) -> String { "sq".into() }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sq<T: Into<f64> + 'static> { side: T, label: Option<&'static str> }

impl<T: Into<f64>> Shape for Sq<T> {
    type Output = f64;
    fn area(&self) -> f64 { self.side.into() * self.side.into() }
}

pub enum Either<L, R> { Left(L), Right(R) }

unsafe fn raw(p: *const u8) -> &'static u8 { unsafe { &*p } }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let closure = |x: i32, y| -> i32 { x + y };
    let r = 0..=10;
    let v: Vec<_> = (0..10).map(|i| i as f64).collect();
    let s = format!("{} {:?}", "a", [1, 2]);
    match (1, 2) {
        (0, y) | (y, 0) => println!("{y}"),
        (a, b) if a > b => {}
        _ => {}
    }
    let Some(x) = opt else { return; };
    if let Ok(n) = res { dbg!(n); }
    while let Some(c) = it.next() {}
    let neg = -x.abs() as i64;
    let t = (1, 2).0;
    my_macro! { a b c };
    println!("{}", x?);
    Ok(())
}
"#;
    probe("rust", rust, packs_rust::reference::parse_events);

    let sql = r"
WITH totals AS (
    SELECT u.id, COUNT(*) AS n
    FROM users u
    LEFT JOIN orders o ON o.user_id = u.id
    WHERE u.active = TRUE AND o.total > 100.5
    GROUP BY u.id
    HAVING COUNT(*) >= 2
)
SELECT * FROM totals
UNION ALL
SELECT id, 0 FROM archived
EXCEPT SELECT id, 0 FROM banned
ORDER BY 1 DESC NULLS LAST
LIMIT 10 OFFSET 5;

ALTER TABLE users ADD COLUMN age INTEGER DEFAULT 0;
DROP TABLE IF EXISTS temp;
CREATE INDEX idx_name ON users (name);
PRAGMA foreign_keys = ON;
BEGIN TRANSACTION;
UPDATE t SET a = a + 1 WHERE EXISTS (SELECT 1);
DELETE FROM t;
COMMIT;

SELECT CASE WHEN a THEN 'x' ELSE 'y' END AS c,
       a % 2, a || b, a IS NOT NULL
FROM t WHERE name LIKE 'a%';
";
    probe("sql", sql, packs_sql::reference::parse_events);

    let toml = r#"
title = "T"
flag = true

[owner]
name = "o"
dob = 1979-05-27T07:32:00Z

[database.ports]
primary = 8000
data = [ ["gamma", "delta"], [1, 2] ]

[[fruit]]
name = "apple"

[fruit.physical]
color = "red"
inline = { a = 1, b.c = [2] }
ml = '''
multi
line'''
"#;
    probe("toml", toml, packs_toml::reference::parse_events);
}
