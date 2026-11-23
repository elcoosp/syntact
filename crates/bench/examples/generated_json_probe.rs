//! Quick allocation + timing probe for the perf report (issue 045).
//! Prints generated vs engine vs reference block counts and a
//! micro-timing on the steady fixture.

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn measured<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let p = dhat::Profiler::new_heap();
    let before = dhat::HeapStats::get();
    let out = f();
    let after = dhat::HeapStats::get();
    drop(p);
    (out, after.total_blocks - before.total_blocks)
}

fn main() {
    let source = bench::steady_state_document();
    eprintln!("fixture: {} bytes", source.len());
    let grammar = gir_syntax::load_grammar(descent_lower::JSON_GRAMMAR).unwrap();
    let lowered = descent_lower::lower(&grammar).unwrap();
    let scanner = lexgen::compile(&grammar).unwrap();

    // Allocations.
    let (_, gen_pe_blocks) = measured(|| generated_json::parse_events(&source));
    let (_, eng_pe_blocks) = measured(|| {
        let tokens = lowered.feed(&scanner, &source);
        descent::parse_events(&lowered.desc, &tokens, &source)
    });
    let (_, gen_tree_blocks) = measured(|| generated_json::parse(&source).unwrap());
    let (_, eng_tree_blocks) = measured(|| {
        let tokens = lowered.feed(&scanner, &source);
        let run = descent::parse_events(&lowered.desc, &tokens, &source);
        descent::green_tree(
            &run,
            &source,
            packs_json::reference::ROOT_KIND,
            lowered.trivia_kind.unwrap(),
        )
        .unwrap()
    });
    let (_, ref_tree_blocks) = measured(|| packs_json::parse(&source).unwrap());

    eprintln!("parse_events blocks:  generated={gen_pe_blocks}  engine={eng_pe_blocks}");
    eprintln!(
        "tree blocks:         generated={gen_tree_blocks}  engine={eng_tree_blocks}  reference={ref_tree_blocks}"
    );

    // Micro-timing (debug build — a sanity check, not the bench number).
    let n = 20;
    let t_gen = std::time::Instant::now();
    for _ in 0..n {
        let _ = generated_json::parse_events(&source);
    }
    let gen_elapsed = t_gen.elapsed() / n;
    let t_eng = std::time::Instant::now();
    let tokens = lowered.feed(&scanner, &source);
    for _ in 0..n {
        let _ = descent::parse_events(&lowered.desc, &tokens, &source);
    }
    let eng_elapsed = t_eng.elapsed() / n;
    let t_ref = std::time::Instant::now();
    for _ in 0..n {
        let _ = packs_json::parse(&source).unwrap();
    }
    let ref_elapsed = t_ref.elapsed() / n;
    eprintln!(
        "debug timing (ns):    generated={gen_elapsed:?}  engine={eng_elapsed:?}  reference={ref_elapsed:?}"
    );
    if eng_elapsed > gen_elapsed {
        eprintln!(
            "generated is {:.2}× faster than engine (debug)",
            eng_elapsed.as_nanos() as f64 / gen_elapsed.as_nanos().max(1) as f64
        );
    }
}
