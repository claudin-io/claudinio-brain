# Evals

Tests prove correctness; evals measure quality. A brain can satisfy every
bitemporal invariant and still be useless if recall never surfaces the answer.

```
cargo run --example eval                      # measure, fail on regression
cargo run --example eval -- --misses          # ...and name the cases still wrong
cargo run --example eval -- --update-baseline # accept the new numbers
```

## The three suites measure different things

**`retrieval.jsonl`** — can recall find the right fact when the question is not
phrased like the fact? Paraphrase, accent drift, capitals, multi-token entity
names, rare identifiers buried in common words, PT and EN.

**`temporal.jsonl`** — what plain RAG gets wrong, and the reason this project
exists. Current value after N changes, value at a past instant, full history,
backdated writes, corrections, retractions, relations that changed.

**`graph.jsonl`** — questions whose answer is **not** lexically similar to the
query and only appears by walking a relation. This suite is what justifies
having a graph at all rather than a plain vector store.

**`alias.jsonl`** — questions that name something by a name the brain was never
given. Two fields drive it: `aliases` declares one up front, and `asked` puts a
question to the brain *with learning on* before the measured query, so the suite
scores what it picked up and not only what it was told. The warmup runs under the
same channels as the case, because a configuration that cannot answer a question
cannot learn from it either — which is why `alias` alone is the weakest row here:
the cases that must be learned need a channel able to find the fact before the
name exists.

## Reading the ablation table

Every suite runs against each channel alone and fused, so the marginal
contribution of each channel is a number rather than an opinion. That is how the
decision to keep or cut a channel gets made -- notably whether the semantic
channel in Passo 5 earns the ~8 MB of model weights it costs, and whether graph
expansion in Passo 6 converts recall depth into precision.

The `no-graph` row is `bm25+alias+semantic`: everything except traversal. It is
permanent, not a one-off measurement, because it is the row the graph channel has
to keep beating to justify staying.

## What these suites cannot measure

Worth stating, so nobody cites a number these files do not contain.

`MIN_ALIAS_COSINE` — the floor that decides whether a question's term is a name
for the entity it resolved to — is **not** calibrated here. Sweeping
0.40/0.50/0.60/0.70 moves no metric on any suite. The floor is still load-bearing;
its effect is visible in `tests/step7_alias.rs`, where removing it lets `mesmo`
become a name for `pgbouncer`. When a constant cannot be priced by these suites,
the source comment says so rather than borrowing their authority.

One case in `alias.jsonl` fails and is expected to keep failing: *a learned name
anchors a walk one hop out*. The name is learned, the walk starts from the right
place and the answer is in the result set (R@5 is 1.000), but the anchor's own
price fact outranks it — three channels agree on the fact the question's words
match, and only the graph channel votes for the fact one hop past it. Passo 6
demotes a traversed *edge* for exactly this reason; nothing yet demotes the
anchor's unrelated facts. Fixing it means introducing a second demotion factor
and tuning it until one visible case flips, which is the overfitting the
anti-overfit rule exists to prevent.

## Baseline as of Passo 7 (lexical + semantic + graph + names)

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

### The one case still wrong

`onde esta a logica do preco_produto_a` returns `produto_c preco 7`. The
structural answer (`regra_desconto definida_em src/pricing/discount.rs`) is
correctly one hop from the anchor and does reach the result set, but the noise
fact wins on lexical agreement: two channels vote for it because the question
literally contains "preco".

Left wrong on purpose. Fixing this specific case means weighting the graph
channel until this one query flips, which is tuning against a visible case --
exactly the overfitting the anti-overfit rule exists to prevent. The honest fix
is more cases of this shape from real transcripts, and a weight decided on all of
them at once.

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

The suites hold ~20 cases each, not the 100+ the plan sketched. Fabricating 80
more by hand would add bulk without adding signal — cases invented to fill a
quota tend to test the implementation that already exists. The intended growth
path is real questions from agent transcripts, added when recall gets one wrong.

`holdout.jsonl` is reserved for a slice that only runs before a release, as a
guard against tuning the ranker against the visible cases.
