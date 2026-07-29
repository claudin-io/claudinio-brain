//! Passo 14: asking about a set.
//!
//! Every read before this one starts from a name. `get` and `history` resolve a
//! subject, `entity` resolves a subject, and `recall` guesses at one -- so the
//! brain could answer "what is this task's status" and could not answer "which
//! tasks are open". That second question is not a harder version of the first; it
//! is a different shape, and no amount of ranking produces it.
//!
//! The claim under test is completeness. `recall` returns what is *relevant* and
//! cannot say what it left out, which is fine for a question and useless for a
//! list. `which` returns what *matches* and says how much matched, so a caller
//! can tell a whole set from the top of one and act on it as a set.

use brain::brain::{Assertion, Brain, Object, Order, WhichQuery};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::When;
use jiff::Timestamp;
use tempfile::TempDir;

fn ts(s: &str) -> Timestamp {
    let s = if s.len() == 10 {
        format!("{s}T00:00:00Z")
    } else {
        s.to_string()
    };
    s.parse().unwrap()
}

/// The clock starts *after* the instants these tests write at, which is the whole
/// point: `which` reads "now" as the instant the question is asked, so a fixture
/// whose facts were all dated in the future would test nothing but the empty list.
fn brain(tmp: &TempDir) -> Brain {
    Brain::init(
        &tmp.path().join("t.db"),
        "which",
        Box::new(StepClock::new(ts("2026-07-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap()
}

/// Records `subject predicate value` as a literal, at an explicit instant.
fn say(b: &Brain, subject: &str, predicate: &str, value: &str, at: &str) {
    b.remember(
        &Assertion::new(subject, predicate, Object::parse_literal(value)).at(ts(at)),
    )
    .unwrap();
}

/// The subject keys of a set answer, which is what a list is actually read for.
fn subjects(b: &Brain, q: &WhichQuery) -> Vec<String> {
    b.which(q)
        .unwrap()
        .facts
        .into_iter()
        .map(|f| f.entity_key)
        .collect()
}

// --- completeness -------------------------------------------------------------

#[test]
fn every_match_is_returned_and_not_the_best_ten() {
    // The anti-`recall` test. Sixty open tasks is past any plausible relevance
    // cut-off, and a list that silently stopped at ten would be worse than no
    // list: the caller would act on it believing it was the whole set.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    for i in 0..60 {
        say(&b, &format!("task_{i:02}"), "status", "open", "2026-02-01");
    }

    let set = b.which(&WhichQuery::new("status").value(Object::text("open"))).unwrap();
    assert_eq!(set.matched, 60);
    assert_eq!(set.facts.len(), 60, "the answer was truncated");
    assert!(!set.truncated);
}

#[test]
fn a_cut_answer_says_how_much_it_cut() {
    // `matched` is the whole reason this type exists. Returning five of sixty is
    // fine; returning five of sixty while implying it was all of them is not.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    for i in 0..60 {
        say(&b, &format!("task_{i:02}"), "status", "open", "2026-02-01");
    }

    let set = b
        .which(&WhichQuery::new("status").value(Object::text("open")).limit(5))
        .unwrap();
    assert_eq!(set.facts.len(), 5);
    assert_eq!(set.matched, 60, "the total was reported as the page size");
    assert!(set.truncated);
}

#[test]
fn a_predicate_nothing_holds_is_an_empty_list_and_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "task_a", "status", "open", "2026-02-01");

    let set = b.which(&WhichQuery::new("assignee")).unwrap();
    assert!(set.facts.is_empty());
    assert_eq!(set.matched, 0);
}

#[test]
fn omitting_the_value_lists_everyone_holding_the_predicate() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "task_a", "status", "open", "2026-02-01");
    say(&b, "task_b", "status", "done", "2026-02-01");
    say(&b, "task_c", "priority", "high", "2026-02-01");

    assert_eq!(
        subjects(&b, &WhichQuery::new("status")),
        ["task_a", "task_b"]
    );
}

// --- the timeline the list inherits -------------------------------------------

#[test]
fn a_subject_that_moved_on_leaves_the_list_it_was_on() {
    // The property a ToDo app throws away on the checkbox click: closing the
    // task does not delete the fact that it was open, it ends it.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "fix_login", "status", "open", "2026-02-01");
    say(&b, "write_docs", "status", "open", "2026-02-01");
    say(&b, "fix_login", "status", "done", "2026-03-01");

    let open = WhichQuery::new("status").value(Object::text("open"));
    let done = WhichQuery::new("status").value(Object::text("done"));
    assert_eq!(subjects(&b, &open), ["write_docs"]);
    assert_eq!(subjects(&b, &done), ["fix_login"]);
}

#[test]
fn the_list_can_be_read_as_it_stood() {
    // No ToDo app can answer this, and it is free here: the closed interval is
    // still on the timeline, so the February list is still a question.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "fix_login", "status", "open", "2026-02-01");
    say(&b, "fix_login", "status", "done", "2026-03-01");

    let open = WhichQuery::new("status").value(Object::text("open"));
    assert_eq!(subjects(&b, &open), Vec::<String>::new(), "open today");
    assert_eq!(
        subjects(&b, &open.clone().when(When::AsOf(ts("2026-02-15")))),
        ["fix_login"],
        "the February list forgot February"
    );
}

#[test]
fn history_includes_the_intervals_that_ended() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "fix_login", "status", "open", "2026-02-01");
    say(&b, "fix_login", "status", "done", "2026-03-01");

    let all = b
        .which(&WhichQuery::new("status").when(When::History))
        .unwrap();
    assert_eq!(all.matched, 2, "the closed interval was dropped");
}

#[test]
fn a_retracted_fact_is_on_no_list_at_all() {
    // It was never true. A list that replayed it would be lying, in exactly the
    // way `recall` refuses to.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "ghost_task", "status", "open", "2026-02-01");
    say(&b, "real_task", "status", "open", "2026-02-01");
    let ghost = b.current("ghost_task", "status").unwrap().unwrap();
    b.retract(ghost.id, Some("filed against the wrong project"))
        .unwrap();

    let open = WhichQuery::new("status").value(Object::text("open"));
    assert_eq!(subjects(&b, &open), ["real_task"]);
    assert_eq!(
        b.which(&open.clone().when(When::History)).unwrap().matched,
        1,
        "a retraction is hidden even from history"
    );
}

#[test]
fn a_fact_dated_in_the_future_is_not_on_todays_list() {
    // "Now" is the instant the question is asked, not "the row nobody has closed
    // yet". A task deferred to next year is the newest thing known about its
    // status and is not something to do today.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "later_task", "status", "open", "2027-06-01");
    say(&b, "now_task", "status", "open", "2026-02-01");

    let open = WhichQuery::new("status").value(Object::text("open"));
    assert_eq!(subjects(&b, &open), ["now_task"]);
    assert_eq!(
        subjects(&b, &open.when(When::AsOf(ts("2027-07-01")))),
        ["later_task", "now_task"],
        "and it is on the list once its time comes"
    );
}

// --- matching the way the fact was written ------------------------------------

#[test]
fn a_number_is_matched_as_a_number() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "produto_a", "preco", "20", "2026-02-01");
    say(&b, "produto_b", "preco", "20", "2026-02-01");
    say(&b, "produto_c", "preco", "35", "2026-02-01");

    let at_twenty = WhichQuery::new("preco").value(Object::parse_literal("20"));
    assert_eq!(subjects(&b, &at_twenty), ["produto_a", "produto_b"]);
}

#[test]
fn a_selection_does_not_blink_when_a_value_becomes_a_relation() {
    // Same reason `kin` joins on `object_text`: an entity-valued object carries
    // its label in that column too, so converting a predicate from strings into
    // real edges must not quietly empty the lists built on it.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "task_a", "status", "open", "2026-02-01");
    say(&b, "task_b", "status", "open", "2026-02-01");
    // The write that teaches the brain `status` is a relation. Everything after
    // it is promoted to an edge automatically.
    b.link("task_c", "status", "open", Some(ts("2026-02-01"))).unwrap();
    say(&b, "task_d", "status", "open", "2026-02-01");

    assert_eq!(
        subjects(&b, &WhichQuery::new("status").value(Object::text("open"))),
        ["task_a", "task_b", "task_c", "task_d"],
        "a literal selection stopped seeing the strings, or the edges"
    );
    assert_eq!(
        subjects(&b, &WhichQuery::new("status").value(Object::entity("open"))),
        ["task_c", "task_d"],
        "an identity selection must match only the real edges"
    );
}

#[test]
fn an_entity_selection_follows_a_declared_alias() {
    // Identity, not spelling: the point of selecting by entity is that the other
    // names of the same thing come along.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    b.link("checkout", "owned_by", "platform_team", Some(ts("2026-02-01")))
        .unwrap();
    b.declare_alias("platform_team", "SRE").unwrap();

    assert_eq!(
        subjects(&b, &WhichQuery::new("owned_by").value(Object::entity("SRE"))),
        ["checkout"]
    );
}

#[test]
fn a_value_naming_nothing_the_brain_knows_is_an_empty_list() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    b.link("checkout", "owned_by", "platform_team", Some(ts("2026-02-01")))
        .unwrap();

    let set = b
        .which(&WhichQuery::new("owned_by").value(Object::entity("nobody_at_all")))
        .unwrap();
    assert!(set.facts.is_empty());
    assert_eq!(set.matched, 0);
}

// --- order --------------------------------------------------------------------

#[test]
fn ordering_by_value_puts_the_nearest_deadline_first() {
    // Why there is no date type: ISO-8601 sorts lexicographically and therefore
    // chronologically, so a deadline is orderable as plain text.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "task_b", "due", "2026-08-15", "2026-02-01");
    say(&b, "task_a", "due", "2026-12-01", "2026-02-01");
    say(&b, "task_c", "due", "2026-03-09", "2026-02-01");

    let by_due = WhichQuery::new("due").order(Order::Value);
    assert_eq!(subjects(&b, &by_due), ["task_c", "task_b", "task_a"]);

    let mut latest = by_due;
    latest.desc = true;
    assert_eq!(subjects(&b, &latest), ["task_a", "task_b", "task_c"]);
}

#[test]
fn ordering_by_since_is_oldest_first() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "newer", "status", "open", "2026-05-01");
    say(&b, "oldest", "status", "open", "2026-01-15");
    say(&b, "middle", "status", "open", "2026-03-01");

    assert_eq!(
        subjects(&b, &WhichQuery::new("status").order(Order::Since)),
        ["oldest", "middle", "newer"]
    );
}

#[test]
fn a_numeric_order_is_numeric_and_not_lexical() {
    // The trap a text sort would fall into: "100" sorts before "9".
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "small", "weight", "9", "2026-02-01");
    say(&b, "large", "weight", "100", "2026-02-01");

    assert_eq!(
        subjects(&b, &WhichQuery::new("weight").order(Order::Value)),
        ["small", "large"]
    );
}

// --- why this is a command and not a graph trick ------------------------------

#[test]
fn the_list_survives_where_the_graph_refuses_to_walk() {
    // `lint` nominates a value several subjects share as a class worth promoting
    // to a node, and promoting it does make `entity <value> --neighbors` read as
    // a list -- right up to the degree cut, where traversal stops going through
    // the hub and the list silently becomes empty. See
    // `step6_graph::a_hub_is_reported_but_never_expanded_through`: the blocking
    // is deliberate, a status value is exactly the shape it blocks, and the
    // failure is an empty answer rather than an error.
    //
    // This is the regression that justifies the command existing.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    for i in 0..60 {
        b.link(&format!("task_{i:02}"), "status", "open", Some(ts("2026-02-01")))
            .unwrap();
    }

    let walked = b.entity("open", When::Now, 2).unwrap().expect("entity");
    assert!(
        walked.neighbours.is_empty(),
        "the hub cut moved; this test's premise needs rechecking"
    );

    let set = b
        .which(&WhichQuery::new("status").value(Object::entity("open")))
        .unwrap();
    assert_eq!(set.matched, 60, "the list the walk could not produce");
}
