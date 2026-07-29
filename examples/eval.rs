//! Measures how well the brain remembers.
//!
//! Tests prove correctness -- does the invariant hold? Evals measure quality --
//! does it find the right fact? Both are needed, and neither substitutes for the
//! other. A brain can satisfy every bitemporal invariant and still be useless if
//! recall never surfaces the answer.
//!
//! Run with `cargo run --bin eval`. Regressions against `evals/baseline.json`
//! fail the run, so improving a number means updating the baseline in the same
//! commit -- the number becomes part of the diff and gets reviewed.
//!
//! `--holdout` adds a suite nothing is tuned against. It is scored the same way
//! and gated the same way, but its cases are never named: `--misses` reports the
//! visible suites only. That asymmetry is the entire point -- a case you can see
//! is a case you can adjust a constant until it passes, and a suite that has had
//! that done to it has stopped being evidence.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::{Channel, RecallQuery};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A fact to load before asking. `id` is a label local to the case, so
/// expectations survive facts being reordered or renumbered.
#[derive(Debug, Deserialize)]
struct EvalFact {
    id: String,
    subject: String,
    predicate: String,
    #[serde(default)]
    num: Option<f64>,
    #[serde(default)]
    text: Option<String>,
    /// Makes this fact a graph edge.
    #[serde(default)]
    entity: Option<String>,
    #[serde(default)]
    at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EvalCase {
    name: String,
    facts: Vec<EvalFact>,
    /// `[entity, alias]` pairs declared before anything is asked.
    #[serde(default)]
    aliases: Vec<[String; 2]>,
    /// Questions asked *with learning on* before the measured one, so a suite can
    /// score what the brain picked up rather than only what it was told. They run
    /// under the same channels as the case itself: a configuration that cannot
    /// answer a question cannot learn from it either, and that dependency belongs
    /// in the ablation table rather than hidden behind a privileged warmup.
    #[serde(default)]
    asked: Vec<String>,
    query: String,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    history: bool,
    /// Labels of the facts that should come back, best first.
    expect: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
struct Metrics {
    cases: usize,
    #[serde(rename = "recall@1")]
    recall_1: f64,
    #[serde(rename = "recall@5")]
    recall_5: f64,
    #[serde(rename = "recall@10")]
    recall_10: f64,
    mrr: f64,
    /// Fraction of cases whose top hit is the expected answer. For an agent this
    /// is the number that matters: it either gets the right fact first or it does
    /// not.
    top1_accuracy: f64,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let update = args.iter().any(|a| a == "--update-baseline");
    let show_misses = args.iter().any(|a| a == "--misses");
    let holdout = args.iter().any(|a| a == "--holdout");

    let suites = ["retrieval", "temporal", "graph", "alias", "kin"];
    let ablations: Vec<(&str, Vec<Channel>)> = vec![
        ("bm25", vec![Channel::Bm25]),
        ("alias", vec![Channel::Alias]),
        ("semantic", vec![Channel::Semantic]),
        ("kin", vec![Channel::Kin]),
        ("bm25+alias", vec![Channel::Bm25, Channel::Alias]),
        // Everything except traversal. This is the row the graph channel has to
        // beat to justify itself, so it stays in the table permanently rather
        // than being measured once and forgotten.
        (
            "no-graph",
            vec![
                Channel::Bm25,
                Channel::Alias,
                Channel::Semantic,
                Channel::Kin,
            ],
        ),
        // Everything except kinship, for the same reason and with the same
        // permanence. A channel that cannot beat the table without it does not
        // deserve to stay in.
        (
            "no-kin",
            vec![
                Channel::Bm25,
                Channel::Alias,
                Channel::Semantic,
                Channel::Graph,
            ],
        ),
        ("all", Channel::ALL.to_vec()),
    ];

    let mut misses: Vec<String> = Vec::new();
    let results = score(&suites, &ablations, show_misses.then_some(&mut misses))?;

    print_table(&results);
    if show_misses {
        println!(
            "\ncases whose top hit is not an expected answer ({}):",
            misses.len()
        );
        for m in &misses {
            println!("  {m}");
        }
    }

    // `None` for the miss list, always: see the module comment. The holdout is
    // scored, gated and reported in aggregate, and never named.
    let held = holdout
        .then(|| score(&["holdout"], &ablations, None))
        .transpose()?;
    if let Some(h) = &held {
        println!("\nholdout -- nothing here is tuned against:");
        print_table(h);
    }

    let mut failed = 0;
    for (results, path) in [
        (Some(&results), "evals/baseline.json"),
        (held.as_ref(), "evals/holdout.json"),
    ] {
        let Some(results) = results else { continue };
        failed += gate(results, std::path::Path::new(path), update)?;
    }
    if failed > 0 {
        anyhow::bail!("{failed} metric(s) regressed");
    }
    Ok(())
}

/// Scores each named suite against every channel configuration.
fn score(
    suites: &[&str],
    ablations: &[(&str, Vec<Channel>)],
    mut misses: Option<&mut Vec<String>>,
) -> anyhow::Result<BTreeMap<String, BTreeMap<String, Metrics>>> {
    let mut results = BTreeMap::new();
    for suite in suites {
        let cases = load_suite(suite)?;
        if cases.is_empty() {
            continue;
        }
        let mut per_channel = BTreeMap::new();
        for (label, channels) in ablations {
            // Only the full configuration reports its misses: those are the cases
            // that are actually still wrong, as opposed to wrong on purpose
            // because a channel was switched off.
            let mut into = match (*label == "all", misses.as_deref_mut()) {
                (true, Some(m)) => Some(m),
                _ => None,
            };
            per_channel.insert(label.to_string(), run_suite(&cases, channels, &mut into)?);
        }
        results.insert(suite.to_string(), per_channel);
    }
    Ok(results)
}

/// Compares one set of results against its committed baseline, or records it.
///
/// Returns how many metrics regressed, rather than failing on the spot, so a run
/// scoring both files reports every drop instead of only the first file's.
fn gate(
    results: &BTreeMap<String, BTreeMap<String, Metrics>>,
    path: &std::path::Path,
    update: bool,
) -> anyhow::Result<usize> {
    if update {
        std::fs::write(path, serde_json::to_string_pretty(results)? + "\n")?;
        println!("baseline updated: {}", path.display());
        return Ok(0);
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        println!(
            "no baseline at {} -- run with --update-baseline.",
            path.display()
        );
        return Ok(0);
    };
    let baseline: BTreeMap<String, BTreeMap<String, Metrics>> = serde_json::from_str(&text)?;
    let regressions = compare(&baseline, results);
    if regressions.is_empty() {
        println!("no regressions against {}.", path.display());
    } else {
        eprintln!("\nREGRESSIONS against {}:", path.display());
        for r in &regressions {
            eprintln!("  {r}");
        }
    }
    Ok(regressions.len())
}

fn load_suite(name: &str) -> anyhow::Result<Vec<EvalCase>> {
    let path = format!("evals/{name}.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        out.push(serde_json::from_str(line).map_err(|e| anyhow::anyhow!("{path}:{}: {e}", i + 1))?);
    }
    Ok(out)
}

fn run_suite(
    cases: &[EvalCase],
    channels: &[Channel],
    misses: &mut Option<&mut Vec<String>>,
) -> anyhow::Result<Metrics> {
    let mut m = Metrics {
        cases: cases.len(),
        ..Default::default()
    };

    for case in cases {
        // A fresh brain per case: no cross-contamination, and the fact ids are
        // reproducible because both the clock and the id generator are seeded.
        //
        // The clock stands after every instant the suites write at and before the
        // 2027 dates the future-dated cases use, which is a constraint and not a
        // detail. "Now" is the instant the question is asked, so a clock before the
        // data would leave every fact not yet true and every default query empty --
        // the suites would go on measuring, and they would be measuring nothing.
        let dir = tempfile::TempDir::new()?;
        let b = Brain::init(
            &dir.path().join("e.db"),
            "eval",
            Box::new(StepClock::new("2026-10-01T00:00:00Z".parse()?, 1000)),
            Box::new(SeededIdGen::new(1)),
        )?;

        let mut labels: BTreeMap<String, i64> = BTreeMap::new();
        for f in &case.facts {
            let object = match (&f.entity, &f.text, f.num) {
                (Some(e), _, _) => Object::entity(e),
                (_, Some(t), _) => Object::text(t),
                (_, _, Some(n)) => Object::num(n),
                _ => anyhow::bail!("{}: fact {} has no value", case.name, f.id),
            };
            let mut a = Assertion::new(&f.subject, &f.predicate, object);
            a.valid_from = f.at.as_deref().map(parse_when).transpose()?;
            let outcome = b.remember(&a)?;
            labels.insert(f.id.clone(), outcome.fact().id);
        }

        for [entity, alias] in &case.aliases {
            b.declare_alias(entity, alias)?;
        }
        for question in &case.asked {
            let warmup = RecallQuery::new(question).limit(10).channels(channels);
            let hits = b.recall(&warmup)?;
            b.learn_alias(question, &hits)?;
        }

        let mut q = RecallQuery::new(&case.query).limit(10).channels(channels);
        if let Some(w) = &case.as_of {
            q = q.as_of(parse_when(w)?);
        }
        if case.history {
            q = q.history();
        }

        let hits = b.recall(&q)?;
        let got: Vec<i64> = hits.iter().map(|h| h.fact.id).collect();
        let want: Vec<i64> = case
            .expect
            .iter()
            .map(|l| {
                labels
                    .get(l)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!("{}: unknown label {l:?}", case.name))
            })
            .collect::<anyhow::Result<_>>()?;

        m.recall_1 += recall_at(&got, &want, 1);
        m.recall_5 += recall_at(&got, &want, 5);
        m.recall_10 += recall_at(&got, &want, 10);
        m.mrr += reciprocal_rank(&got, &want);
        // A case that expects nothing is answered correctly by returning nothing.
        // Scoring that as a top-1 failure -- which this did until the graph
        // channel's miss list made it visible -- understates every suite that
        // contains a "the brain should stay quiet" case.
        let correct = match want.is_empty() {
            true => got.is_empty(),
            false => got.first().is_some_and(|top| want.contains(top)),
        };
        if correct {
            m.top1_accuracy += 1.0;
        } else if let Some(out) = misses.as_deref_mut() {
            let top = hits
                .first()
                .map_or("(nothing)".to_string(), |h| h.fact.statement.clone());
            out.push(format!(
                "{}\n      asked: {}\n      got:   {top}",
                case.name, case.query
            ));
        }
    }

    let n = cases.len() as f64;
    for v in [
        &mut m.recall_1,
        &mut m.recall_5,
        &mut m.recall_10,
        &mut m.mrr,
        &mut m.top1_accuracy,
    ] {
        *v = (*v / n * 1000.0).round() / 1000.0;
    }
    Ok(m)
}

fn recall_at(got: &[i64], want: &[i64], k: usize) -> f64 {
    if want.is_empty() {
        return 1.0;
    }
    let found = want
        .iter()
        .filter(|w| got.iter().take(k).any(|g| g == *w))
        .count();
    found as f64 / want.len() as f64
}

fn reciprocal_rank(got: &[i64], want: &[i64]) -> f64 {
    got.iter()
        .position(|g| want.contains(g))
        .map_or(0.0, |i| 1.0 / (i + 1) as f64)
}

fn parse_when(s: &str) -> anyhow::Result<Timestamp> {
    brain::cli::parse_when(s)
}

fn print_table(results: &BTreeMap<String, BTreeMap<String, Metrics>>) {
    println!(
        "{:<12} {:<12} {:>5} {:>7} {:>7} {:>8} {:>7} {:>6}",
        "suite", "channels", "n", "R@1", "R@5", "R@10", "MRR", "top1"
    );
    println!("{}", "-".repeat(72));
    for (suite, per_channel) in results {
        for (channels, m) in per_channel {
            println!(
                "{:<12} {:<12} {:>5} {:>7.3} {:>7.3} {:>8.3} {:>7.3} {:>6.3}",
                suite,
                channels,
                m.cases,
                m.recall_1,
                m.recall_5,
                m.recall_10,
                m.mrr,
                m.top1_accuracy
            );
        }
    }
}

/// Small tolerance so floating-point noise does not fail a run, while any real
/// drop does.
const TOLERANCE: f64 = 0.001;

fn compare(
    baseline: &BTreeMap<String, BTreeMap<String, Metrics>>,
    current: &BTreeMap<String, BTreeMap<String, Metrics>>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (suite, per_channel) in baseline {
        for (channels, want) in per_channel {
            let Some(got) = current.get(suite).and_then(|c| c.get(channels)) else {
                out.push(format!("{suite}/{channels}: missing from this run"));
                continue;
            };
            for (metric, w, g) in [
                ("recall@1", want.recall_1, got.recall_1),
                ("recall@5", want.recall_5, got.recall_5),
                ("recall@10", want.recall_10, got.recall_10),
                ("mrr", want.mrr, got.mrr),
                ("top1", want.top1_accuracy, got.top1_accuracy),
            ] {
                if g < w - TOLERANCE {
                    out.push(format!("{suite}/{channels} {metric}: {w:.3} -> {g:.3}"));
                }
            }
        }
    }
    out
}
