# Evals

Tests prove correctness; evals measure quality. A brain can satisfy every
bitemporal invariant and still be useless if recall never surfaces the answer.

```
cargo run --example eval                      # measure, fail on regression
cargo run --example eval -- --misses          # ...and name the cases still wrong
cargo run --example eval -- --holdout         # ...and score the suite nothing is tuned against
cargo run --example eval -- --update-baseline # accept the new numbers
```

## The suites measure different things

**`retrieval.jsonl`** — can recall find the right fact when the question is not
phrased like the fact? Paraphrase, accent drift, capitals, multi-token entity
names, rare identifiers buried in common words, PT and EN.

**`temporal.jsonl`** — what plain RAG gets wrong, and the reason this project
exists. Current value after N changes, value at a past instant, full history,
backdated writes, corrections, retractions, relations that changed.

**`graph.jsonl`** — questions whose answer is **not** lexically similar to the
query and only appears by walking a relation. This suite is what justifies
having a graph at all rather than a plain vector store.

**`kin.jsonl`** — questions whose useful fact sits on an entity *nothing*
connects to the one asked about: no edge, no shared words, only a value the two
happen to hold in common. Where `graph.jsonl` justifies having a graph, this
justifies looking past it — in a real brain almost nothing has an edge to its
siblings, and twenty vouchers each recording `is_a voucher_sazonal` are a cohort
nobody ever drew.

**`alias.jsonl`** — questions that name something by a name the brain was never
given. Two fields drive it: `aliases` declares one up front, and `asked` puts a
question to the brain *with learning on* before the measured query, so the suite
scores what it picked up and not only what it was told. The warmup runs under the
same channels as the case, because a configuration that cannot answer a question
cannot learn from it either — which is why `alias` alone is the weakest row here:
the cases that must be learned need a channel able to find the fact before the
name exists.

**`holdout.jsonl`** — the control. Every suite above is visible: `--misses` names
the cases still wrong, so a constant can be nudged until one of them flips, and
the visible numbers end up measuring the ranker *and* how long someone stared at
them. The holdout runs only under `--holdout`, keeps its numbers in
`evals/holdout.json` so the everyday `--update-baseline` cannot absorb them, and
**never names a failing case**. That asymmetry is the whole mechanism: a case you
can see is a case you can tune against.

The rule that comes with it: if the holdout regresses and you need to know why,
move the case into a visible suite and say so in the pull request. A holdout case
you have looked at is not a holdout case any more, and pretending otherwise is
worse than losing it.

## Reading the ablation table

Every suite runs against each channel alone and fused, so the marginal
contribution of each channel is a number rather than an opinion. That is how the
decision to keep or cut a channel gets made -- notably whether the semantic
channel in Passo 5 earns the ~8 MB of model weights it costs, and whether graph
expansion in Passo 6 converts recall depth into precision.

The `no-graph` row is everything except traversal, and `no-kin` is everything
except kinship. Both are permanent, not one-off measurements, because they are the
rows those channels have to keep beating to justify staying.

## What these suites cannot measure

Worth stating, so nobody cites a number these files do not contain.

`MIN_ALIAS_COSINE` — the floor that decides whether a question's term is a name
for the entity it resolved to — is **not** calibrated here. Sweeping
0.40/0.50/0.60/0.70 moves no metric on any suite. The floor is still load-bearing;
its effect is visible in `tests/step7_alias.rs`, where removing it lets `mesmo`
become a name for `pgbouncer`. When a constant cannot be priced by these suites,
the source comment says so rather than borrowing their authority.

This section used to carry a case in `alias.jsonl` marked *expected to keep
failing* — a learned name anchoring a walk one hop out, where the anchor's own
price fact outranked the answer. The stated reason for leaving it was sound: the
only visible fix was a second demotion factor tuned until that one case flipped,
which is the overfitting the anti-overfit rule exists to prevent.

Passo 8 fixed it, by answering the objection rather than ignoring it. Five more
cases of the same two shapes went into the visible suites first, a 24-case
holdout was written before the mechanism existed, and the two rules were swept
across every suite at once. See *Baseline as of Passo 8* below.

### What kinship bought

| suite | row | R@1 | R@5 | R@10 | MRR | top-1 |
|---|---|---|---|---|---|---|
| kin | `no-kin` | 0.433 | 0.967 | 1.000 | 0.850 | 0.700 |
| kin | `all` | **0.733** | 0.967 | 1.000 | **1.000** | **1.000** |
| holdout | `no-kin` | 0.889 | 1.000 | 1.000 | 0.938 | 0.958 |
| holdout | `all` | **0.931** | 1.000 | 1.000 | **0.958** | **1.000** |

Nothing moves on `retrieval`, `temporal`, `graph` or `alias` — not one figure —
which is the shape a channel that expands rather than answers should have. The
holdout row is the one added in Passo 8, and it is the more interesting of the
two: kinship pays there as well, on cases written long after the channel was.

The instructive part is *which* numbers moved. R@5 and R@10 are identical with
the channel off, so the lateral facts were reachable the whole time and what
kinship supplies is precision, not depth. That is the same story as traversal in
Passo 6, and it is worth stating because the opposite was the guess going in:
"reaches things nothing connects" sounds like a recall feature and measured as a
ranking one.

`kin` alone scores 0.048 and 0.050 on `temporal` and `retrieval`. That is correct
and not a defect — kinship answers no question by itself, it only says where else
to look, and a suite of direct questions is exactly where that is worth nothing.

One case used to be left wrong on purpose here too: *a property the anchor does
not have, on a voucher sharing its discount*. The lateral fact was found and the
anchor's own unrelated fact still outranked it. Passo 8's predicate rule fixed
it — the question says `expira`, and only one of the two candidates has that
predicate.

## Baseline as of Passo 8 (focus: demoting what the question did not ask for)

```
suite        channels         n     R@1     R@5     R@10     MRR   top1
alias        alias           10   0.600   0.600    0.600   0.500  0.600
alias        bm25            10   0.850   1.000    1.000   0.825  0.900
alias        semantic        10   0.650   1.000    1.000   0.725  0.700
alias        bm25+alias      10   0.750   1.000    1.000   0.775  0.800
alias        no-graph        10   0.850   1.000    1.000   0.825  0.900
alias        no-kin          10   0.950   1.000    1.000   0.900  1.000
alias        all             10   0.950   1.000    1.000   0.900  1.000
graph        alias           12   0.083   0.083    0.083   0.083  0.083
graph        bm25            12   0.250   0.667    0.667   0.444  0.250
graph        semantic        12   0.083   0.833    0.917   0.328  0.083
graph        bm25+alias      12   0.083   0.667    0.667   0.340  0.083
graph        no-graph        12   0.083   0.917    1.000   0.394  0.083
graph        no-kin          12   1.000   1.000    1.000   1.000  1.000
graph        all             12   1.000   1.000    1.000   1.000  1.000
kin          alias           10   0.433   0.467    0.467   0.700  0.700
kin          bm25            10   0.633   0.967    1.000   0.950  0.900
kin          semantic        10   0.633   0.967    1.000   0.950  0.900
kin          bm25+alias      10   0.433   0.967    1.000   0.850  0.700
kin          no-graph        10   0.733   0.967    1.000   1.000  1.000
kin          no-kin          10   0.433   0.967    1.000   0.850  0.700
kin          all             10   0.733   0.967    1.000   1.000  1.000
retrieval    alias           22   0.705   0.727    0.727   0.682  0.727
retrieval    bm25            22   0.977   1.000    1.000   0.955  1.000
retrieval    semantic        22   0.977   1.000    1.000   0.955  1.000
retrieval    bm25+alias      22   0.977   1.000    1.000   0.955  1.000
retrieval    no-graph        22   0.977   1.000    1.000   0.955  1.000
retrieval    no-kin          22   0.977   1.000    1.000   0.955  1.000
retrieval    all             22   0.977   1.000    1.000   0.955  1.000
temporal     alias           21   0.893   0.952    0.952   0.905  0.952
temporal     bm25            21   0.909   1.000    1.000   0.952  1.000
temporal     semantic        21   0.909   1.000    1.000   0.952  1.000
temporal     bm25+alias      21   0.909   1.000    1.000   0.952  1.000
temporal     no-graph        21   0.909   1.000    1.000   0.952  1.000
temporal     no-kin          21   0.909   1.000    1.000   0.952  1.000
temporal     all             21   0.909   1.000    1.000   0.952  1.000

holdout      alias           24   0.556   0.604    0.604   0.583  0.625
holdout      bm25            24   0.681   0.958    0.958   0.795  0.750
holdout      semantic        24   0.681   0.958    1.000   0.783  0.750
holdout      bm25+alias      24   0.639   0.958    0.958   0.764  0.708
holdout      no-graph        24   0.681   1.000    1.000   0.797  0.750
holdout      no-kin          24   0.889   1.000    1.000   0.938  0.958
holdout      all             24   0.931   1.000    1.000   0.958  1.000
```

**`--misses` is empty.** Every visible suite reaches 1.000 top-1, including the
two cases earlier sections of this file carried as permanently wrong.

Ten cases were added to the visible suites in the same step, so the suites are
harder than the Passo 7 numbers they replace, not just better scored. Six cases
were failing after they landed — three of each shape — and all six flipped.

### The two rules, and what each one bought

Both are multiplicative on the fused score, like `BRIDGE_DEMOTION` before them,
and both are inert unless the question said something specific enough to apply
them. They were swept independently, and the striking part is how cleanly they
separate: neither moves a single figure on the suites the other fixes.

| | graph top-1 | alias top-1 | kin top-1 | holdout top-1 |
|---|---|---|---|---|
| neither | 0.750 | 0.800 | 0.900 | 0.958 |
| off-topic only | **1.000** | 0.800 | 0.900 | **1.000** |
| unasked-predicate only | 0.750 | **1.000** | **1.000** | 0.958 |
| both | **1.000** | **1.000** | **1.000** | **1.000** |

`retrieval` and `temporal` are 1.000 in all four rows.

**Off topic.** A question naming an entity the brain knows is a question with an
address on it; a fact about an entity that is neither named nor reached arrived
by sharing a *word*. `preco_produto_a` and `produto_c preco 7` share two tokens
once FTS5 splits the underscore, which was enough for the noise fact to collect
two channel votes and win.

**The predicate nobody asked for.** When a question uses one of the brain's own
predicate keys it has said what it wants in the brain's vocabulary. That signal
already broke ties inside the walk and inside kinship; this is the same signal at
fusion level, where it can settle an argument *between* channels.

### What the holdout says, and what it does not

This is the number worth trusting, because nothing was tuned against it: **top-1
0.958 → 1.000, R@1 0.889 → 0.931, MRR 0.931 → 0.958**, on 24 cases in a domain
none of the visible suites use, written before the mechanisms existed.

That verdict belongs to the off-topic rule alone. The predicate rule moves the
holdout by exactly nothing at any setting — its cases of that shape already
ranked right — so what the holdout says about it is that it breaks nothing, which
is a weaker claim than generalizing, and the two should not be blurred together.

### What the sweeps could not decide

Both constants are flat from 0.25 to 0.95, and only 1.0 — the rule switched off —
differs. These suites priced the *rule*, not the number.

| factor | graph top-1 | alias top-1 | holdout top-1 |
|---|---|---|---|
| 1.00 (off) | 0.750 | 0.800 | 0.958 |
| 0.95 | 1.000 | 1.000 | 1.000 |
| 0.75 | 1.000 | 1.000 | 1.000 |
| **0.50** | 1.000 | 1.000 | 1.000 |
| 0.25 | 1.000 | 1.000 | 1.000 |

0.5 was chosen for a reason these suites cannot show, and the source comments say
so rather than claiming the sweep picked it: the contests being settled here are
RRF near-ties, which yield to any nudge at all, and in a larger brain the losing
fact can trail by much more than five percent. A factor that only works when the
margin is already negligible is one that quietly stops working as the brain
grows.

## Baseline as of Passo 7 (lexical + semantic + graph + names)

Superseded by Passo 8 above, which also added ten cases, so the `n` columns
differ and the rows are not directly comparable.

```
suite        channels         n     R@1     R@5     R@10     MRR   top1
alias        alias            8   0.625   0.625    0.625   0.500  0.625
alias        bm25             8   0.688   1.000    1.000   0.698  0.750
alias        semantic         8   0.563   1.000    1.000   0.650  0.625
alias        bm25+alias       8   0.688   1.000    1.000   0.729  0.750
alias        no-graph         8   0.813   1.000    1.000   0.792  0.875
alias        all              8   0.813   1.000    1.000   0.792  0.875
graph        alias            8   0.000   0.000    0.000   0.000  0.000
graph        bm25             8   0.125   0.750    0.750   0.375  0.125
graph        semantic         8   0.000   0.875    0.875   0.237  0.000
graph        bm25+alias       8   0.000   0.750    0.750   0.313  0.000
graph        no-graph         8   0.000   1.000    1.000   0.358  0.000
graph        all              8   0.875   1.000    1.000   0.938  0.875
retrieval    alias           20   0.675   0.700    0.700   0.650  0.700
retrieval    bm25            20   0.975   1.000    1.000   0.950  1.000
retrieval    semantic        20   0.825   1.000    1.000   0.842  0.850
retrieval    bm25+alias      20   0.975   1.000    1.000   0.950  1.000
retrieval    no-graph        20   0.975   1.000    1.000   0.950  1.000
retrieval    all             20   0.975   1.000    1.000   0.950  1.000
temporal     alias           21   0.893   0.952    0.952   0.905  0.952
temporal     bm25            21   0.813   1.000    1.000   0.905  0.905
temporal     semantic        21   0.909   1.000    1.000   0.952  1.000
temporal     bm25+alias      21   0.909   1.000    1.000   0.952  1.000
temporal     no-graph        21   0.909   1.000    1.000   0.952  1.000
temporal     all             21   0.909   1.000    1.000   0.952  1.000
```

### What traversal bought

| suite | top-1 without graph | top-1 with graph |
|---|---|---|
| retrieval | 1.000 | 1.000 |
| temporal | 1.000 | 1.000 |
| graph | 0.000 | **0.875** |

Recall@10 on the graph suite was **already 1.000 before this step**. The answer
was in the candidate set the whole time and could not get to the top, because the
fact that lexically matches "de que pais vem o produto_a" is the *link*
(`produto_a fornecido_por acme`), not the answer (`acme pais brasil`) one hop
past it. Expansion alone does not fix that: with traversal on and bridge demotion
off, top-1 is still 0.000.

**Two mechanisms, measured separately.** Expansion supplies the candidate;
demoting the traversed edge is what lets the candidate win:

| bridge demotion | graph R@1 | graph MRR | graph top-1 |
|---|---|---|---|
| 1.00 (off) | 0.000 | 0.479 | 0.000 |
| 0.75 | 0.625 | 0.792 | 0.625 |
| **0.50** | 0.750 | 0.875 | 0.750 |
| 0.25 | 0.750 | 0.875 | 0.750 |

0.50 is the mildest value that reaches the ceiling. Retrieval and temporal do not
move at any setting, which is the expected shape: those suites barely contain
edges, so there is nothing to demote.

The remaining 0.750-to-0.875 came from demoting *chains*: reaching a contact
through a supplier makes both hops roads, even though the middle entity
contributes no answer of its own.

### The one case still wrong — and how it was eventually fixed

`onde esta a logica do preco_produto_a` returned `produto_c preco 7`. The
structural answer (`regra_desconto definida_em src/pricing/discount.rs`) was
correctly one hop from the anchor and did reach the result set, but the noise
fact won on lexical agreement: two channels voted for it because the question
literally contains "preco".

It was left wrong on purpose, and the reason given here was:

> Fixing this specific case means weighting the graph channel until this one
> query flips, which is tuning against a visible case — exactly the overfitting
> the anti-overfit rule exists to prevent. The honest fix is more cases of this
> shape from real transcripts, and a weight decided on all of them at once.

Passo 8 did the honest version. The prescription is kept here in full because it
is the more useful half of the record: an entry that says *what would make this
fixable* is what let a later step fix it without arguing about whether it should.

### Does the semantic channel still earn its 7.3 MB?

Passo 5 kept it on one piece of evidence: it lifted graph Recall@10 from 0.750 to
1.000, the input graph expansion needed. Passo 6 was the test of whether that
recall converted into precision, and it did -- graph top-1 went 0.000 to 0.875.
The channel stays, and now on a stronger footing than "it might help later".

### A measurement bug the miss list exposed

`--misses` (added in this step, prints the cases whose top hit is wrong) showed
two cases that were being counted as top-1 failures while behaving exactly right:
they *expect* nothing, and returning nothing was scored as failure. Fixed, so
retrieval and temporal top-1 read 1.000 rather than 0.950/0.952. Nothing about
the system improved -- the measurement stopped being wrong.

## Earlier baselines

### Passo 5 (lexical + semantic)

```
graph        all              8   0.000   1.000    1.000   0.358  0.000
retrieval    all             20   0.975   1.000    1.000   0.950  0.950
temporal     all             21   0.909   1.000    1.000   0.952  0.952
```

The semantic channel's contribution was entirely in recall depth: it lifted graph
R@10 from 0.750 to 1.000 while leaving top-1 at 0.000 in every suite. Note that
the top-1 figures here predate the empty-expectation fix above.

Also worth keeping: `semantic` alone matches the fused lexical channels on the
temporal suite (0.909 R@1, 0.952 MRR). Static embeddings do more work here than
their ~82%-of-MiniLM MTEB reputation suggests, probably because facts are short
and structured rather than prose.

### Passo 4 (lexical only)

```
graph        bm25             8   0.125   0.750    0.750   0.375  0.125
graph        bm25+alias       8   0.000   0.750    0.750   0.313  0.000
retrieval    bm25            20   0.975   1.000    1.000   0.950  0.950
temporal     bm25            21   0.813   1.000    1.000   0.905  0.857
temporal     bm25+alias      21   0.909   1.000    1.000   0.952  0.952
```

Findings that still hold: fusion earns its keep on the temporal suite (0.813 and
0.893 alone, 0.909 fused); the alias channel adds nothing on retrieval and
survives only on its temporal contribution; and fusion *hurts* graph top-1
(0.125 bm25-only to 0.000 fused) because the alias channel correctly pins the
named entity and surfaces the link fact while the answer sits one hop further
on. That 0.000 was the number Passo 6 had to move, and did.

## Growing the datasets

The suites hold 10–24 cases each, not the 100+ the plan sketched. Fabricating 80
more by hand would add bulk without adding signal — cases invented to fill a
quota tend to test the implementation that already exists. The intended growth
path is real questions from agent transcripts, added when recall gets one wrong.

Passo 8 added ten, and they are the exception that proves the rule rather than a
departure from it: they were not invented to fill a quota, they were written to
turn two *known* failures into a population big enough to decide a constant
against. Cases added for that reason are worth having; cases added to make a
number look thorough are not.

`holdout.jsonl` exists as of Passo 8 and runs under `--holdout`, in CI on every
code change rather than only before a release — a check whose whole purpose is to
disagree with the visible numbers finds that out too late if it runs once a
release. What it is and the rule for touching it are described at the top of this
file.
